//! Per-app capture on macOS 14.4+ with Core Audio process taps.
//!
//! A `CATapDescription` mixes the chosen processes' output down to mono,
//! a private aggregate device wraps the tap, and an IOProc on that device
//! feeds the usual [`Plumbing`]. The apps keep playing to their normal
//! output. The first run asks for "System Audio Recording" permission
//! (attributed to the terminal); without it the tap delivers silence.
//!
//! Apps such as browsers play from helper processes that come and go, so
//! the capture thread rescans every couple of seconds and retargets the
//! tap when the set of matching processes changes.

use std::ffi::{c_void, CStr};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _, Result};
use crossbeam_channel::bounded;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::AllocAnyThread;
use objc2_core_audio::{
    kAudioAggregateDeviceIsPrivateKey, kAudioAggregateDeviceMainSubDeviceKey,
    kAudioAggregateDeviceNameKey, kAudioAggregateDeviceSubDeviceListKey,
    kAudioAggregateDeviceTapAutoStartKey, kAudioAggregateDeviceTapListKey,
    kAudioAggregateDeviceUIDKey, kAudioDevicePropertyDeviceUID,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyProcessObjectList,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject,
    kAudioProcessPropertyBundleID, kAudioProcessPropertyIsRunningOutput, kAudioProcessPropertyPID,
    kAudioSubDeviceUIDKey, kAudioSubTapDriftCompensationKey, kAudioSubTapUIDKey,
    kAudioTapPropertyDescription, kAudioTapPropertyFormat, AudioDeviceCreateIOProcID,
    AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioHardwareCreateAggregateDevice, AudioHardwareCreateProcessTap,
    AudioHardwareDestroyAggregateDevice, AudioHardwareDestroyProcessTap,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize, AudioObjectID,
    AudioObjectPropertyAddress, AudioObjectPropertySelector, AudioObjectSetPropertyData,
    CATapDescription,
};
use objc2_core_audio_types::{
    kAudioFormatFlagIsNonInterleaved, AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSUUID};
use tracing::{info, warn};

use crate::{push_rt, AppInfo, Capture, CaptureParts, Plumbing};

const RESCAN: Duration = Duration::from_secs(2);

/// Audio clients Core Audio knows about (anything that has opened an
/// output or input), with whether each is playing right now.
pub fn list_apps() -> Result<Vec<AppInfo>> {
    let mut out = Vec::new();
    for obj in get_vec::<AudioObjectID>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyProcessObjectList,
    )? {
        let Ok(pid) = get::<i32>(obj, kAudioProcessPropertyPID) else {
            continue;
        };
        if pid == std::process::id() as i32 {
            continue;
        }
        out.push(AppInfo {
            pid: pid as u32,
            name: process_name(pid),
            id: get_string(obj, kAudioProcessPropertyBundleID)
                .map(|s| s.to_string())
                .unwrap_or_default(),
            playing: get::<u32>(obj, kAudioProcessPropertyIsRunningOutput).unwrap_or(0) != 0,
            object: obj,
        });
    }
    Ok(out)
}

/// Process objects matching `want`: a PID, or a case-insensitive
/// substring of the process name or bundle id.
fn matching(want: &str) -> Result<Vec<AppInfo>> {
    Ok(list_apps()?
        .into_iter()
        .filter(|a| a.matches(want))
        .collect())
}

pub fn open(want: &str) -> Result<Capture> {
    // The tap, aggregate device and IOProc are torn down by the same
    // thread that made them, after `Capture` drops.
    let want = want.to_string();
    let (ready_tx, ready_rx) = bounded::<Result<(u32, String, CaptureParts)>>(1);
    let thread = std::thread::Builder::new()
        .name("vox-process-tap".into())
        .spawn(move || {
            let mut tap = match Tap::start(&want) {
                Ok(tap) => tap,
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            let stop = tap.parts.as_ref().expect("parts").stop.clone();
            let parts = tap.parts.take().expect("parts");
            let _ = ready_tx.send(Ok((tap.rate, tap.label(), parts)));
            let mut last_scan = Instant::now();
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(50));
                if last_scan.elapsed() >= RESCAN {
                    last_scan = Instant::now();
                    if let Err(e) = tap.retarget() {
                        warn!("process tap rescan: {e:#}");
                    }
                }
            }
        })?;
    let (rate, name, parts) = ready_rx.recv().context("process tap thread died")??;
    Ok(parts.finish(rate, name, thread))
}

struct RtState {
    producer: rtrb::Producer<f32>,
    overruns: Arc<AtomicU64>,
    non_interleaved: bool,
}

struct Tap {
    want: String,
    desc: Retained<CATapDescription>,
    procs: Vec<AudioObjectID>,
    tap: AudioObjectID,
    device: AudioObjectID,
    io_proc: AudioDeviceIOProcID,
    state: *mut RtState,
    rate: u32,
    parts: Option<CaptureParts>,
}

impl Tap {
    fn start(want: &str) -> Result<Self> {
        let apps = matching(want)?;
        if apps.is_empty() {
            bail!(
                "no audio app matching {want:?}; it must have played sound since it started (try `apps`)"
            );
        }
        let procs: Vec<AudioObjectID> = apps.iter().map(|a| a.object).collect();
        info!(
            apps = ?apps.iter().map(|a| format!("{} ({})", a.name, a.pid)).collect::<Vec<_>>(),
            "creating process tap"
        );
        let desc = unsafe {
            let d = CATapDescription::initMonoMixdownOfProcesses(
                CATapDescription::alloc(),
                &ns_numbers(&procs),
            );
            d.setName(&NSString::from_str(&format!("vox_scribe {want}")));
            d.setPrivate(true);
            d
        };
        let mut tap = Tap {
            want: want.to_string(),
            desc,
            procs,
            tap: 0,
            device: 0,
            io_proc: None,
            state: std::ptr::null_mut(),
            rate: 0,
            parts: None,
        };
        // On error, Drop releases whatever was created so far.
        let mut tap_id: AudioObjectID = 0;
        check(
            unsafe { AudioHardwareCreateProcessTap(Some(&tap.desc), &mut tap_id) },
            "AudioHardwareCreateProcessTap (needs macOS 14.4+)",
        )?;
        tap.tap = tap_id;

        let format = get::<AudioStreamBasicDescription>(tap.tap, kAudioTapPropertyFormat)?;
        let non_interleaved = format.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0;
        let channels = if non_interleaved {
            1
        } else {
            format.mChannelsPerFrame.max(1) as usize
        };
        tap.rate = format.mSampleRate as u32;
        info!(
            rate = tap.rate,
            channels = format.mChannelsPerFrame,
            "process tap format"
        );

        tap.device = create_aggregate(&tap.desc)?;

        let plumbing = Plumbing::new(tap.rate, channels);
        let (producer, parts) = plumbing.into_parts();
        tap.state = Box::into_raw(Box::new(RtState {
            producer,
            overruns: parts.overruns.clone(),
            non_interleaved,
        }));
        tap.parts = Some(parts);
        let mut io_proc: AudioDeviceIOProcID = None;
        check(
            unsafe {
                AudioDeviceCreateIOProcID(
                    tap.device,
                    Some(io_proc_cb),
                    tap.state.cast(),
                    NonNull::from(&mut io_proc),
                )
            },
            "AudioDeviceCreateIOProcID",
        )?;
        tap.io_proc = io_proc;
        check(
            unsafe { AudioDeviceStart(tap.device, tap.io_proc) },
            "AudioDeviceStart",
        )?;
        Ok(tap)
    }

    fn label(&self) -> String {
        format!("app {:?} ({} process tap)", self.want, self.procs.len())
    }

    /// Point the tap at the current set of matching processes.
    fn retarget(&mut self) -> Result<()> {
        let mut procs: Vec<AudioObjectID> =
            matching(&self.want)?.iter().map(|a| a.object).collect();
        procs.sort_unstable();
        let mut current = self.procs.clone();
        current.sort_unstable();
        if procs == current || procs.is_empty() {
            return Ok(());
        }
        info!(
            from = current.len(),
            to = procs.len(),
            "retargeting process tap"
        );
        unsafe { self.desc.setProcesses(&ns_numbers(&procs)) };
        let ptr: *const CATapDescription = Retained::as_ptr(&self.desc);
        set(self.tap, kAudioTapPropertyDescription, &ptr)?;
        self.procs = procs;
        Ok(())
    }
}

impl Drop for Tap {
    fn drop(&mut self) {
        unsafe {
            if self.device != 0 {
                if self.io_proc.is_some() {
                    AudioDeviceStop(self.device, self.io_proc);
                    AudioDeviceDestroyIOProcID(self.device, self.io_proc);
                }
                AudioHardwareDestroyAggregateDevice(self.device);
            }
            if self.tap != 0 {
                AudioHardwareDestroyProcessTap(self.tap);
            }
            if !self.state.is_null() {
                drop(Box::from_raw(self.state));
            }
        }
    }
}

unsafe extern "C-unwind" fn io_proc_cb(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    _output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let state = &mut *(client as *mut RtState);
    let list = input.as_ref();
    let buffers = std::slice::from_raw_parts(list.mBuffers.as_ptr(), list.mNumberBuffers as usize);
    // A non-interleaved tap would give one buffer per channel; the
    // plumbing was told mono, so take the first.
    let take = if state.non_interleaved {
        1
    } else {
        buffers.len()
    };
    for b in buffers.iter().take(take) {
        if b.mData.is_null() {
            continue;
        }
        let samples = std::slice::from_raw_parts(
            b.mData as *const f32,
            b.mDataByteSize as usize / std::mem::size_of::<f32>(),
        );
        push_rt(
            &mut state.producer,
            &state.overruns,
            samples.iter().copied(),
        );
    }
    0
}

/// A private aggregate device whose only input is the tap, clocked by
/// the default output device (as in Apple's sample code).
fn create_aggregate(desc: &CATapDescription) -> Result<AudioObjectID> {
    let tap_uid = unsafe { desc.UUID() }.UUIDString();
    let agg_uid = NSUUID::new().UUIDString();
    let key = |k: &CStr| NSString::from_str(k.to_str().expect("utf8 key"));

    let sub_tap = NSDictionary::<NSString, AnyObject>::from_slices(
        &[
            &*key(kAudioSubTapUIDKey),
            &*key(kAudioSubTapDriftCompensationKey),
        ],
        &[&*tap_uid as &AnyObject, &*NSNumber::new_bool(true)],
    );
    let taps = NSArray::from_slice(&[&*sub_tap]);

    let mut keys = vec![
        key(kAudioAggregateDeviceNameKey),
        key(kAudioAggregateDeviceUIDKey),
        key(kAudioAggregateDeviceIsPrivateKey),
        key(kAudioAggregateDeviceTapAutoStartKey),
        key(kAudioAggregateDeviceTapListKey),
    ];
    let name = NSString::from_str("vox_scribe tap");
    let yes = NSNumber::new_bool(true);
    let mut values: Vec<&AnyObject> = vec![&name, &agg_uid, &yes, &yes, &taps];

    let output_uid = get::<AudioObjectID>(
        kAudioObjectSystemObject as AudioObjectID,
        kAudioHardwarePropertyDefaultOutputDevice,
    )
    .and_then(|dev| get_string(dev, kAudioDevicePropertyDeviceUID));
    let sub_dev;
    let sub_devs;
    if let Ok(uid) = &output_uid {
        sub_dev = NSDictionary::<NSString, AnyObject>::from_slices(
            &[&*key(kAudioSubDeviceUIDKey)],
            &[&**uid as &AnyObject],
        );
        sub_devs = NSArray::from_slice(&[&*sub_dev]);
        keys.push(key(kAudioAggregateDeviceMainSubDeviceKey));
        values.push(&**uid);
        keys.push(key(kAudioAggregateDeviceSubDeviceListKey));
        values.push(&sub_devs);
    } else {
        warn!("no default output device; aggregate runs on the tap's clock");
    }
    let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
    let dict = NSDictionary::<NSString, AnyObject>::from_slices(&key_refs, &values);

    let mut device: AudioObjectID = 0;
    // NSDictionary is toll-free bridged to CFDictionary.
    let cf = unsafe { &*(Retained::as_ptr(&dict) as *const objc2_core_foundation::CFDictionary) };
    check(
        unsafe { AudioHardwareCreateAggregateDevice(cf, NonNull::from(&mut device)) },
        "AudioHardwareCreateAggregateDevice",
    )?;
    Ok(device)
}

fn ns_numbers(ids: &[AudioObjectID]) -> Retained<NSArray<NSNumber>> {
    let nums: Vec<Retained<NSNumber>> = ids.iter().map(|&id| NSNumber::new_u32(id)).collect();
    NSArray::from_retained_slice(&nums)
}

fn process_name(pid: i32) -> String {
    let mut buf = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let n = unsafe { libc::proc_pidpath(pid, buf.as_mut_ptr().cast(), buf.len() as u32) };
    if n <= 0 {
        return format!("pid {pid}");
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
    match path.rsplit_once('/') {
        Some((_, name)) => name.to_string(),
        None => path,
    }
}

fn check(status: i32, what: &str) -> Result<()> {
    if status != 0 {
        let code = status.to_be_bytes();
        let fourcc = if code.iter().all(|c| c.is_ascii_graphic()) {
            format!(" '{}'", String::from_utf8_lossy(&code))
        } else {
            String::new()
        };
        bail!("{what} failed: OSStatus {status}{fourcc}");
    }
    Ok(())
}

fn address(selector: AudioObjectPropertySelector) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn get<T: Copy>(obj: AudioObjectID, selector: AudioObjectPropertySelector) -> Result<T> {
    let mut addr = address(selector);
    let mut value = std::mem::MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>() as u32;
    check(
        unsafe {
            AudioObjectGetPropertyData(
                obj,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(value.as_mut_ptr().cast()),
            )
        },
        "AudioObjectGetPropertyData",
    )?;
    Ok(unsafe { value.assume_init() })
}

fn get_vec<T: Copy>(obj: AudioObjectID, selector: AudioObjectPropertySelector) -> Result<Vec<T>> {
    let mut addr = address(selector);
    let mut size = 0u32;
    check(
        unsafe {
            AudioObjectGetPropertyDataSize(
                obj,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
            )
        },
        "AudioObjectGetPropertyDataSize",
    )?;
    let count = size as usize / std::mem::size_of::<T>();
    let mut out: Vec<T> = Vec::with_capacity(count);
    if count == 0 {
        return Ok(out);
    }
    check(
        unsafe {
            AudioObjectGetPropertyData(
                obj,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::new_unchecked(out.as_mut_ptr().cast()),
            )
        },
        "AudioObjectGetPropertyData",
    )?;
    unsafe { out.set_len(size as usize / std::mem::size_of::<T>()) };
    Ok(out)
}

/// A CFString property (returned +1, bridged to NSString).
fn get_string(
    obj: AudioObjectID,
    selector: AudioObjectPropertySelector,
) -> Result<Retained<NSString>> {
    let ptr = get::<*mut NSString>(obj, selector)?;
    unsafe { Retained::from_raw(ptr) }.context("null string property")
}

fn set<T>(obj: AudioObjectID, selector: AudioObjectPropertySelector, value: &T) -> Result<()> {
    let mut addr = address(selector);
    check(
        unsafe {
            AudioObjectSetPropertyData(
                obj,
                NonNull::from(&mut addr),
                0,
                std::ptr::null(),
                std::mem::size_of::<T>() as u32,
                NonNull::from(value).cast(),
            )
        },
        "AudioObjectSetPropertyData",
    )
}
