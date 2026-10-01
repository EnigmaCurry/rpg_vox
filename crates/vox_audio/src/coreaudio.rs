//! CoreAudio input through cpal.

use std::sync::atomic::Ordering;

use anyhow::{anyhow, bail, Context as _, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Sample, SampleFormat};
use crossbeam_channel::bounded;
use tracing::warn;

use crate::{push_rt, Backend, Capture, DeviceInfo, OpenOptions, Plumbing};

pub struct CoreAudio;

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .or_else(|_| d.id().map(|id| id.to_string()))
        .unwrap_or_else(|_| "unknown".into())
}

fn device_id(d: &cpal::Device) -> String {
    d.id().map(|id| id.to_string()).unwrap_or_default()
}

fn find_device(host: &cpal::Host, want: Option<&str>) -> Result<cpal::Device> {
    let Some(want) = want else {
        return host
            .default_input_device()
            .context("no default input device");
    };
    let needle = want.to_lowercase();
    let mut fuzzy = None;
    for d in host.input_devices()? {
        if device_id(&d) == want {
            return Ok(d);
        }
        if fuzzy.is_none() && device_name(&d).to_lowercase().contains(&needle) {
            fuzzy = Some(d);
        }
    }
    fuzzy.ok_or_else(|| anyhow!("no input device matching {want:?} (try `devices`)"))
}

impl Backend for CoreAudio {
    fn name(&self) -> &'static str {
        "coreaudio"
    }

    fn list_devices(&self) -> Result<Vec<DeviceInfo>> {
        let host = cpal::default_host();
        let default = host.default_input_device().map(|d| device_id(&d));
        let mut out = Vec::new();
        for d in host.input_devices()? {
            let id = device_id(&d);
            out.push(DeviceInfo {
                is_default: default.as_deref() == Some(id.as_str()),
                name: device_name(&d),
                id,
            });
        }
        Ok(out)
    }

    fn open(&self, opts: &OpenOptions) -> Result<Capture> {
        if opts.virtual_sink {
            bail!("--virtual-sink is PipeWire-only; on macOS route apps through a loopback device such as BlackHole and pick it with --device");
        }
        // cpal::Stream is !Send on macOS, so it lives on its own thread
        // for the whole capture.
        let want = opts.device.clone();
        let (ready_tx, ready_rx) = bounded::<Result<(u32, String, crate::CaptureParts)>>(1);
        let thread = std::thread::Builder::new()
            .name("vox-coreaudio".into())
            .spawn(move || {
                let started = (|| -> Result<_> {
                    let host = cpal::default_host();
                    let device = find_device(&host, want.as_deref())?;
                    let name = device_name(&device);
                    let config = device
                        .default_input_config()
                        .context("default input config")?;
                    let rate = config.sample_rate();
                    let channels = config.channels() as usize;
                    let format = config.sample_format();
                    let plumbing = Plumbing::new(rate, channels);
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
