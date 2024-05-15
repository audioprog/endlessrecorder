// Copyright (C) audioProg@webls.de
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License,
// or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see http://www.gnu.org/licenses/.

extern crate cpal;
extern crate ringbuf;
extern crate hound;
extern crate chrono;
extern crate confy;
extern crate serde;

mod mainconfig; 

use mainconfig::MainConfig;
use cpal::{traits::{DeviceTrait, HostTrait, StreamTrait}, StreamConfig};
use std::{env, io::{self, Write}, path::{Path, PathBuf}};
use hound::{WavSpec, WavWriter};
use std::sync::{atomic::AtomicBool, atomic::Ordering, mpsc::channel, Arc};
use std::thread;
use chrono::Local;
use std::error::Error;

const SAMPLE_RATE: u32 = 48000;
const CACHE_SIZE_IN_BYTES: usize = 512 * 1024 * 1024; // 512 MB
const CACHE_FLUSH_SIZE: usize = CACHE_SIZE_IN_BYTES / 2; // Half the cache size

fn main() -> Result<(), Box<dyn Error>> {
    let app_name = env!("CARGO_PKG_NAME");
    println!("{} Version {}", &app_name, env!("CARGO_PKG_VERSION"));
    let current_dir = std::env::current_dir()?;
    println!("Current working directory: {:?}", current_dir);

    let args: Vec<String> = env::args().collect();
    let reinit = args.contains(&"reinit".to_string());

    let host = cpal::default_host();

    let global_config_dir = get_global_config_path(&app_name).join("default-config.ron");
    println!("Global: {:?}", global_config_dir);

    let conffile = confy::get_configuration_file_path(&app_name, None)?;
    println!("{:#?}", conffile);

    let cfg: MainConfig = if reinit {
        init_config(&app_name)?
    } else {
        let cfg_in: MainConfig = confy::load(app_name, None)?;
        if cfg_in.selected_device.is_some() {
            cfg_in
        } else if global_config_dir.exists() {
            let global_path: &Path = global_config_dir.as_path();
            confy::load_path::<MainConfig>(global_path)?
        } else {
            init_config(&app_name)?
        }
    };

    println!("{:#?}", cfg.selected_device.as_ref().unwrap());

    let device = host.devices()?.find(|d| d.name().ok().as_ref() == cfg.selected_device.as_ref()).expect("Saved device not found");

    let mut max_sample_rate: u32 = 0;
	let mut supported_channels: u16 = 1;
    // Output of the supported formats
    let supported_formats= device.supported_input_configs()?;
    for config_range  in supported_formats {
        println!("Supported format: {:?}", config_range );
        if config_range.max_sample_rate().0 > max_sample_rate {
			supported_channels = config_range.channels();
            max_sample_rate = config_range.max_sample_rate().0;
        }
    }

    // Use the preferred sample rate if it is available, otherwise use the highest possible sample rate.
    let selected_sample_rate = if max_sample_rate >= cpal::SampleRate(SAMPLE_RATE).0 {
        cpal::SampleRate(SAMPLE_RATE).0
    } else {
        max_sample_rate
    };

    let config = StreamConfig {
        channels: supported_channels,
        sample_rate: cpal::SampleRate(selected_sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    println!("Selected: {:?}", config);

    // Flag for the clean termination of the program
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    let sr = running.clone();

    // Thread für das Erkennen von Tastendrücken
    thread::spawn(move || {
        println!("Press Enter to exit...");

        let mut input = String::new();
        io::stdout().flush().unwrap(); // Ensures that the above text is output immediately.
        io::stdin().read_line(&mut input).unwrap();
        r.store(false, Ordering::SeqCst);
    });
    
    let (sender, receiver) = channel();

    // Thread for caching samples
    let cache_sender = sender.clone();

    println!("Record start");

    let stream = device.build_input_stream(
        &config,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut samples = Vec::with_capacity(data.len());
            samples.extend_from_slice(data);
            if let Err(_) = cache_sender.send(samples) {
                if !sr.load(Ordering::SeqCst) {
                    return;
                }
            }
        },
        |err| eprintln!("Error during stream: {:?}", err),
        None,
    ).unwrap();
    stream.play().unwrap();

    // Thread for writing to WAV files
    let write_thread = start_write_thread(receiver, running.clone(), supported_channels, selected_sample_rate);

    write_thread.join().unwrap();

    drop(stream);

    println!("Record stop");

    Ok(())
}


// Function for selecting the device
fn init_config(app_name: &str) -> Result<mainconfig::MainConfig, Box<dyn Error>> {
    let host = cpal::default_host();
    let devices: Vec<cpal::Device> = host.input_devices()?.collect();
    println!("Please select an audio device:");
    for (index, device) in devices.iter().enumerate() {
        println!("{}: {}", index + 1, device.name()?);
    }

    let mut device_index = String::new();
    io::stdout().flush().unwrap(); // Make sure that the text is output immediately.
    io::stdin().read_line(&mut device_index).expect("Error when reading the input");
    let device_index: usize = device_index.trim().parse::<usize>().expect("Please enter a valid number") - 1;
    
    let selected_device = devices.get(device_index).expect("Invalid device number selected");
    let device_name = selected_device.name()?;
    let new_cfg = MainConfig {
        selected_device: Some(device_name.clone()),
    };
    confy::store(app_name, None, new_cfg.clone())?;
    Ok(new_cfg)
}


fn get_global_config_path(app_name: &str) -> PathBuf {
    #[cfg(target_os = "linux")]
    return PathBuf::from("/etc/").join(app_name);

    #[cfg(target_os = "windows")]
    return PathBuf::from("C:\\ProgramData\\").join(app_name);

    #[cfg(target_os = "macos")]
    return PathBuf::from("/Library/Application Support/").join(app_name);
}


fn start_write_thread(
    receiver: std::sync::mpsc::Receiver<Vec<f32>>,
    running: Arc<AtomicBool>,
    supported_channels: u16,
    selected_sample_rate: u32,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut cached_samples = Vec::new();
        let mut filename = format!("{}.wav", Local::now().format("%Y-%m-%d_%H-%M-%S"));
        println!("File {}", filename);

        loop {
            if let Ok(samples) = receiver.try_recv() {
                cached_samples.extend(samples);

                if cached_samples.len() * std::mem::size_of::<f32>() >= CACHE_FLUSH_SIZE || !running.load(Ordering::SeqCst) {
                    
                    let spec = WavSpec {
                        channels: supported_channels,
                        sample_rate: selected_sample_rate,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    };
                    let mut writer = WavWriter::create(filename.clone(), spec).unwrap();

                    for sample in cached_samples.drain(..) {
                        writer.write_sample(sample).unwrap();
                    }
                    writer.finalize().unwrap();
                    
                    println!("finished file {}", filename);

                    if !running.load(Ordering::SeqCst) {
                        break;
                    }

                    filename = format!("{}.wav", Local::now().format("%Y-%m-%d_%H-%M-%S"));
                    println!("File {}", filename);
                }
            }

            if !running.load(Ordering::SeqCst) {
                if cached_samples.len() * std::mem::size_of::<f32>() > 0 {
                    let spec = WavSpec {
                        channels: supported_channels,
                        sample_rate: selected_sample_rate,
                        bits_per_sample: 32,
                        sample_format: hound::SampleFormat::Float,
                    };
                    let mut writer = WavWriter::create(filename.clone(), spec).unwrap();

                    for sample in cached_samples.drain(..) {
                        writer.write_sample(sample).unwrap();
                    }
                    writer.finalize().unwrap();
                    
                    println!("finished file {}", filename);
                }
                break;
            }
        }
        println!("wave cache file ends");
    })
}