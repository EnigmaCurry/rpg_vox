//! Input through cpal: CoreAudio on macOS; WASAPI on Windows; on Linux,
//! ALSA where PipeWire isn't running (see [`crate::pipewire::running`]),
//! which on a PulseAudio system reaches its default source through the
//! ALSA plugin.
//!
//! On Windows an input stream opened on an *output* device records what
//! that device is playing (WASAPI loopback), so the output devices are
//! offered for capture alongside the microphones — that is how scribe
//! transcribes a call coming out of the speakers, the job `--virtual-sink`
//! and `--app` do on PipeWire.

use std::sync::atomic::Ordering;

use anyhow::{anyhow, bail, Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use crossbeam_channel::bounded;
use tracing::warn;

#[cfg(target_os = "macos")]
use crate::AppInfo;
use crate::{push_rt, Backend, Capture, DeviceInfo, Mix, OpenOptions, Plumbing};

pub struct Cpal;

/// `--device loopback`: whatever Windows is playing through its default
/// output, without having to name the device. Windows only.
pub const LOOPBACK: &str = "loopback";

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .or_else(|_| d.id().map(|id| id.to_string()))
        .unwrap_or_else(|_| "unknown".into())
}

fn device_id(d: &cpal::Device) -> String {
    d.id().map(|id| id.to_string()).unwrap_or_default()
}

/// A device to capture from, and whether capturing it means WASAPI
/// loopback: an input stream on an output device, which records what
/// that device plays. Always `false` off Windows.
struct Chosen {
    device: cpal::Device,
    loopback: bool,
}

/// Everything that can be captured, microphones first. On Windows the
/// output devices follow, for loopback capture.
fn capturable(host: &cpal::Host) -> Result<Vec<Chosen>> {
    // Nothing is appended off Windows, where there is no loopback.
    #[cfg_attr(not(target_os = "windows"), allow(unused_mut))]
    let mut out: Vec<Chosen> = host
        .input_devices()?
        .map(|device| Chosen {
            device,
            loopback: false,
        })
        .collect();
    #[cfg(target_os = "windows")]
    out.extend(host.output_devices()?.map(|device| Chosen {
        device,
        loopback: true,
    }));
    Ok(out)
}

fn find_device(host: &cpal::Host, want: Option<&str>) -> Result<Chosen> {
    let Some(want) = want else {
        return host
            .default_input_device()
            .map(|device| Chosen {
                device,
                loopback: false,
            })
            .context("no default input device");
    };
    #[cfg(target_os = "windows")]
    if want.eq_ignore_ascii_case(LOOPBACK) {
        return host
            .default_output_device()
            .map(|device| Chosen {
                device,
                loopback: true,
            })
            .context("no default output device to capture");
    }
    let needle = want.to_lowercase();
    let mut fuzzy = None;
    for chosen in capturable(host)? {
        if device_id(&chosen.device) == want {
            return Ok(chosen);
        }
        if fuzzy.is_none() && device_name(&chosen.device).to_lowercase().contains(&needle) {
            fuzzy = Some(chosen);
        }
    }
    fuzzy.ok_or_else(|| anyhow!("no input device matching {want:?} (try `devices`)"))
}

impl Backend for Cpal {
    fn name(&self) -> &'static str {
        if cfg!(target_os = "linux") {
            "alsa"
        } else if cfg!(target_os = "windows") {
            "wasapi"
        } else {
            "coreaudio"
        }
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let host = cpal::default_host();
        let default_in = host.default_input_device().map(|d| device_id(&d));
        // Only Windows lists outputs, where they are the loopback
        // sources; asking elsewhere would open a device for nothing.
        let default_out = if cfg!(target_os = "windows") {
            host.default_output_device().map(|d| device_id(&d))
        } else {
            None
        };
        let mut out = Vec::new();
        for chosen in capturable(&host)? {
            let id = device_id(&chosen.device);
            let default = if chosen.loopback {
                &default_out
            } else {
                &default_in
            };
            out.push(DeviceInfo {
                is_default: default.as_deref() == Some(id.as_str()),
                loopback: chosen.loopback,
                name: device_name(&chosen.device),
                // Finding out would mean asking every device for its
                // config just to list it; `open` reads the real count
                // off the one device it opens.
                channels: 0,
                id,
            });
        }
        Ok(out)
    }

    #[cfg(target_os = "macos")]
    fn list_apps(&self) -> Result<Vec<AppInfo>> {
        crate::process_tap::list_apps()
    }

    fn open(&self, opts: &OpenOptions) -> Result<Capture> {
        if opts.virtual_sink {
            if cfg!(target_os = "linux") {
                bail!("--virtual-sink needs PipeWire, which isn't running");
            }
            if cfg!(target_os = "windows") {
                bail!("--virtual-sink is PipeWire-only; on Windows `--device {LOOPBACK}` transcribes what the speakers are playing");
            }
            bail!("--virtual-sink is PipeWire-only; on macOS use --app, or route apps through a loopback device such as BlackHole and pick it with --device");
        }
        #[cfg(target_os = "macos")]
        if let Some(app) = &opts.app {
            return crate::process_tap::open(app);
        }
        #[cfg(target_os = "linux")]
        if opts.app.is_some() {
            bail!("--app needs PipeWire, which isn't running");
        }
        #[cfg(target_os = "windows")]
        if opts.app.is_some() {
            bail!("WASAPI can't single out one app; `--device {LOOPBACK}` captures everything the speakers play");
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        if opts.app.is_some() {
            bail!("--app is not supported on this OS");
        }
        // cpal::Stream is !Send, so it lives on its own thread for the
        // whole capture.
        let want = opts.device.clone();
        let channel = opts.channel;
        let (ready_tx, ready_rx) = bounded::<Result<(u32, String, crate::CaptureParts)>>(1);
        let thread = std::thread::Builder::new()
            .name("vox-cpal".into())
            .spawn(move || {
                let started = (|| -> Result<_> {
                    let host = cpal::default_host();
                    let Chosen { device, loopback } = find_device(&host, want.as_deref())?;
                    let name = match loopback {
                        true => format!("{} (loopback)", device_name(&device)),
                        false => device_name(&device),
                    };
                    let name = match channel {
                        Some(c) => format!("{name}:{}", crate::channel_label(c)),
                        None => name,
                    };
                    // A WASAPI output device has no input format of its
                    // own — `default_input_config` on one is an error.
                    // What an input stream on it hands back is what the
                    // device is rendering, in the rendering format.
                    let config = match loopback {
                        true => device
                            .default_output_config()
                            .context("default output config")?,
                        false => device
                            .default_input_config()
                            .context("default input config")?,
                    };
                    let rate = config.sample_rate();
                    let channels = config.channels() as usize;
                    let format = config.sample_format();
                    let mix =
                        Mix::of(channel, channels).with_context(|| format!("device {name:?}"))?;
                    let plumbing = Plumbing::new(rate, channels, mix);
                    let (mut producer, parts) = plumbing.into_parts();
                    let overruns = parts.overruns.clone();
                    let err_fn = |e: cpal::Error| warn!("audio stream error: {e}");
                    let cfg: cpal::StreamConfig = config.into();
                    let stream = match format {
                        SampleFormat::F32 => device.build_input_stream(
                            cfg,
                            move |d: &[f32], _: &_| {
                                push_rt(&mut producer, &overruns, d.iter().copied())
                            },
                            err_fn,
                            None,
                        )?,
                        SampleFormat::I16 => device.build_input_stream(
                            cfg,
                            move |d: &[i16], _: &_| {
                                push_rt(
                                    &mut producer,
                                    &overruns,
                                    d.iter().map(|s| s.to_sample::<f32>()),
                                )
                            },
                            err_fn,
                            None,
                        )?,
                        SampleFormat::I32 => device.build_input_stream(
                            cfg,
                            move |d: &[i32], _: &_| {
                                push_rt(
                                    &mut producer,
                                    &overruns,
                                    d.iter().map(|s| s.to_sample::<f32>()),
                                )
                            },
                            err_fn,
                            None,
                        )?,
                        other => bail!("unsupported input sample format {other}"),
                    };
                    stream.play().context("start input stream")?;
                    Ok((stream, rate, name, parts))
                })();
                match started {
                    Ok((stream, rate, name, parts)) => {
                        let stop = parts.stop.clone();
                        let _ = ready_tx.send(Ok((rate, name, parts)));
                        while !stop.load(Ordering::Relaxed) {
                            std::thread::sleep(std::time::Duration::from_millis(50));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })?;
        let (rate, name, parts) = ready_rx.recv().context("audio thread died")??;
        Ok(parts.finish(rate, name, thread))
    }
}
