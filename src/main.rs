extern crate cpal;
extern crate crossterm;
extern crate ringbuf;
extern crate hound;
extern crate chrono;

use cpal::{traits::{DeviceTrait, HostTrait, StreamTrait}, StreamConfig};
use crossterm::event;
use hound::{WavSpec, WavWriter};
use std::sync::{atomic::AtomicBool, atomic::Ordering, mpsc::channel, Arc};
use std::thread;
use chrono::Local;
use std::error::Error;

const SAMPLE_RATE: u32 = 48000;
const CACHE_SIZE_IN_BYTES: usize = 512 * 1024 * 1024; // 512 MB
const CACHE_FLUSH_SIZE: usize = CACHE_SIZE_IN_BYTES / 2; // Half the cache size

fn main() -> Result<(), Box<dyn Error>> {
    println!("{} Version {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
    let current_dir = std::env::current_dir()?;
    println!("Current working directory: {:?}", current_dir);

    let host = cpal::default_host();
    let device = host.default_input_device().expect("No input device available");

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
        while r.load(Ordering::SeqCst) {
            if event::poll(std::time::Duration::from_millis(100)).unwrap() {
                if let Ok(true) = event::read().map(|e| matches!(e, event::Event::Key(_))) {
                    r.store(false, Ordering::SeqCst);
                }
            }
        }
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
    let write_thread = thread::spawn(move || {
        let mut cached_samples = Vec::new();

        println!("wave cache start");

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

                    if !running.load(Ordering::SeqCst) {
                        break;
                    }
                }
                break;
            }
        }
        println!("wave cache file ends");
    });
	
	println!("End with any key");

    write_thread.join().unwrap();

    drop(stream);

    println!("Record stop");

    Ok(())
}
