use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use ringbuf::{Consumer, LocalRb, Rb, SharedRb};
use std::io::Write;
use std::mem::MaybeUninit;
use std::sync::Arc;
use std::time::{Duration, Instant};
use std::{cmp, thread};
use whisper_rs::{
    print_system_info, FullParams, SamplingStrategy, WhisperContext, WhisperState, WhisperToken,
};

const LATENCY_MS: f32 = 4000.0;
const NUM_ITERS: usize = 2;
const NUM_ITERS_SAVED: usize = 2;
const MODEL_NAME: &str = "ggml-medium.en.bin";

pub fn run_whisper() -> Result<(), &'static str> {
    // load a context and model
    let ctx = WhisperContext::new(
        format!("/home/tine/projects/whisper.cpp/models/{}", MODEL_NAME).as_str(),
    )
    .expect("failed to load model");
    // make a state
    let mut state = ctx.create_state().expect("failed to create state");

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .expect("failed to get default input device");

    println!("Device: {}", device.name().unwrap());

    let config = device
        .supported_input_configs()
        .unwrap()
        .find(|c| c.channels() == 1 && c.sample_format() == cpal::SampleFormat::F32)
        .expect("failed to find supported input config")
        .with_sample_rate(cpal::SampleRate(16000))
        .config();

    println!("Config: {:?}", config);

    println!("{}", print_system_info());

    let latency_frames = (LATENCY_MS / 1_000.0) * config.sample_rate.0 as f32;
    let latency_samples = latency_frames as usize * config.channels as usize;
    let sampling_freq = config.sample_rate.0 as f32;

    // The buffer to share samples
    let ring = SharedRb::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                let mut output_fell_behind = false;
                for &sample in data {
                    if producer.push(sample).is_err() {
                        output_fell_behind = true;
                    }
                }
                if output_fell_behind {
                    eprintln!("output stream fell behind: try increasing latency");
                }
            },
            move |err| {
                eprintln!("an error occurred on stream: {}", err);
            },
            Some(Duration::from_secs(10)),
        )
        .expect("failed to build stream");

    stream.play().expect("failed to play stream");

    process_loop(
        &mut consumer,
        latency_samples,
        sampling_freq,
        &mut state,
        &ctx,
    )
    .expect("failed to process loop");

    Ok(())
}

pub fn process_loop(
    consumer: &mut Consumer<f32, Arc<SharedRb<f32, Vec<MaybeUninit<f32>>>>>,
    latency_samples: usize,
    sampling_freq: f32,
    state: &mut WhisperState,
    ctx: &WhisperContext,
) -> Result<(), &'static str> {
    let mut transcription = String::new();

    // Variables used across loop iterations
    let mut iter_samples = LocalRb::new(latency_samples * NUM_ITERS * 2);
    let mut iter_num_samples = LocalRb::new(NUM_ITERS);
    let mut iter_tokens = LocalRb::new(NUM_ITERS_SAVED);
    for _ in 0..NUM_ITERS {
        iter_num_samples
            .push(0)
            .expect("Error initailizing iter_num_samples");
    }

    consumer.pop_iter().count();
    let mut start_time = Instant::now();

    let mut num_chars_to_delete = 0;
    let mut loop_num = 0;
    let mut words = "".to_owned();
    loop {
        loop_num += 1;

        // Only run every LATENCY_MS
        let duration = start_time.elapsed();
        let latency = Duration::from_millis(LATENCY_MS as u64);
        println!(
            "Duration: {} Latency: {}",
            duration.as_millis(),
            latency.as_millis()
        );
        if duration < latency {
            let sleep_time = latency - duration;
            thread::sleep(sleep_time);
        } else {
            println!("Classification got behind. It took to long. Try using a smaller model and/or more threads");
        }
        start_time = Instant::now();

        // Collect the samples
        let samples: Vec<_> = consumer.pop_iter().collect();
        let num_samples_to_delete = iter_num_samples
            .push_overwrite(samples.len())
            .expect("Error num samples to delete is off");
        for _ in 0..num_samples_to_delete {
            iter_samples.pop();
        }
        iter_samples.push_iter(&mut samples.into_iter());
        let (head, tail) = iter_samples.as_slices();
        let current_samples = [head, tail].concat();

        // Get tokens to be deleted
        if loop_num > 1 {
            let num_tokens = state.full_n_tokens(0).expect("Error getting num tokens");
            let token_time_end = state
                .full_get_segment_t1(0)
                .expect("Error getting token time");
            let token_time_per_ms =
                token_time_end as f32 / (LATENCY_MS * cmp::min(loop_num, NUM_ITERS) as f32); // token times are not a value in ms, they're 150 per second
            let ms_per_token_time = 1.0 / token_time_per_ms;

            let mut tokens_saved = vec![];
            // Skip beginning and end token
            for i in 1..num_tokens - 1 {
                let token = state
                    .full_get_token_data(0, i)
                    .expect("Error getting token data");
                let token_t0_ms = token.t0 as f32 * ms_per_token_time;
                let ms_to_delete = num_samples_to_delete as f32 / (sampling_freq / 1000.0);

                // Save tokens for whisper context
                if (loop_num > NUM_ITERS) && token_t0_ms < ms_to_delete {
                    tokens_saved.push(token.id);
                }
            }
            num_chars_to_delete = words.chars().count();
            if loop_num > NUM_ITERS {
                num_chars_to_delete -= tokens_saved
                    .iter()
                    .map(|x| ctx.token_to_str(*x).expect("Error"))
                    .collect::<String>()
                    .chars()
                    .count();
            }
            iter_tokens.push_overwrite(tokens_saved);
        }

        // Make the model params
        let (head, tail) = iter_tokens.as_slices();
        let tokens = [head, tail]
            .concat()
            .into_iter()
            .flatten()
            .collect::<Vec<WhisperToken>>();
        let mut params = whisper_params();
        params.set_tokens(&tokens);

        // Run the model
        state
            .full(params, &current_samples)
            .expect("failed to convert samples");

        // Update the words on screen
        if num_chars_to_delete != 0 {
            transcription = transcription
                .split_at(transcription.len() - num_chars_to_delete)
                .0
                .to_string();
        }
        let num_tokens = state.full_n_tokens(0).expect("Error getting num tokens");
        words = (1..num_tokens - 1)
            .map(|i| {
                state
                    .full_get_token_text(0, i)
                    .map_err(|_| "".to_string())
                    .expect("")
            })
            .collect::<String>();
        transcription += &words;
        print!("{}", words);
        std::io::stdout().flush().unwrap();
    }
}

pub fn whisper_params<'a>() -> FullParams<'a, 'a> {
    let mut params = FullParams::new(SamplingStrategy::default());
    params.set_print_progress(false);
    params.set_print_special(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    params.set_language(Some("en"));
    params.set_token_timestamps(true);
    params.set_duration_ms(LATENCY_MS as i32);
    params.set_no_context(true);
    params.set_n_threads(10);

    params
}
