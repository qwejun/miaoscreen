//! A headless frame publisher for the round secondary screen.
//!
//! This binary keeps everything above the drawing boundary — configuration,
//! input, runtime, the evaluated [`bongocat_render::RenderSnapshot`] — and
//! replaces only the drawing: snapshots leave through [`catscreen::MagneoSink`]
//! and the device rasterises them at the panel's own resolution. No window is
//! created, so the desktop shows nothing, and no GPU is touched, so the picture
//! does not depend on this machine's renderer.
//!
//! The contract with the runtime is the same one the desktop overlay obeys,
//! including the model-commit acknowledgement: a consumer that never reports it
//! leaves every later command deferred forever.

#![forbid(unsafe_code)]
// A command-line tool's observable output is its log, and this one has no window
// and no product UI to report through. The workspace denies `print_stderr` so
// library code stays quiet; a binary with no other channel is the case that rule
// is not about.
#![allow(clippy::print_stderr)]

mod bindings;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bongocat_model::{ModelId, ModelPackageLimits, PresetModelCatalog};
use bongocat_platform::WindowsInputService;
use bongocat_render::{ModelCommitFeedback, ModelCommitOutcome, RenderConsumer};
use bongocat_runtime::{RuntimeClient, RuntimeCommand, RuntimeOwner};
use catscreen::MagneoSink;

const COMMAND_CAPACITY: usize = 64;
const TIMEOUT: Duration = Duration::from_secs(5);
const DEFAULT_ADDRESS: &str = "127.0.0.1:7391";
const DEFAULT_MODEL: &str = "standard";
const POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Set `CATSCREEN_TRACE_KEYS=1` to trace the pressed-key set in the frame
/// stream. Pressed keys are not user content in the sense the logging rules
/// forbid — they are what the cat is reacting to — but the trace is still off by
/// default because it writes one line per keystroke.
fn trace_keys() -> bool {
    static TRACE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *TRACE.get_or_init(|| std::env::var_os("CATSCREEN_TRACE_KEYS").is_some())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("catscreen: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse()?;

    let catalog = PresetModelCatalog::open(&options.models, ModelPackageLimits::default())?;
    let model_id = ModelId::parse(options.model.clone())?;
    let committed = catalog.load(&model_id)?;
    let input_bindings = bindings::for_committed_model(&committed);
    eprintln!(
        "catscreen: model {} at {}",
        model_id.as_str(),
        committed.root().display()
    );

    let (owner, consumer) = RuntimeOwner::start_with_rendering(true, COMMAND_CAPACITY);
    let client = owner.client();
    client
        .wait_for_revision(1, TIMEOUT)
        .ok_or("the runtime never published its first snapshot")?;

    let sequence = client.send(RuntimeCommand::ActivateModelWithBindings {
        model: Arc::new(committed),
        input_bindings: Arc::new(input_bindings),
    })?;
    let prepared = client
        .wait_for_model_preparation(sequence, TIMEOUT)
        .ok_or("the runtime never finished preparing the model")?;
    if let Some(failure) = prepared.last_command_failure
        && failure.sequence == sequence
    {
        return Err(format!("model activation failed: {}", failure.code).into());
    }

    acknowledge_model_commit(&client, &consumer)?;

    let input = WindowsInputService::start(
        owner.input_producer(),
        owner.cursor_producer(),
        owner.gamepad_axis_producer(),
    )?;
    eprintln!("catscreen: input service ready");

    let mut sink = MagneoSink::bind(&options.address)?;
    eprintln!("catscreen: listening on {}", sink.destination());
    let published = publish(&consumer, &mut sink);
    let _ = input.stop();
    let _ = owner.shutdown(TIMEOUT);
    published
}

/// Answers the pending model commit, then waits for the runtime to accept it.
///
/// Without this the worker keeps `pending_model` set and defers every command
/// that is not an input event, so the picture freezes on the first frame.
fn acknowledge_model_commit(
    client: &RuntimeClient,
    consumer: &RenderConsumer,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + TIMEOUT;
    let token = loop {
        if let Some(frame) = consumer.take_latest()
            && let Some(token) = frame.model_commit
        {
            break token;
        }
        if Instant::now() >= deadline {
            return Err("the runtime never published a model commit token".into());
        }
        thread::sleep(POLL_INTERVAL);
    };

    consumer.report_model_commit(ModelCommitFeedback {
        token,
        outcome: ModelCommitOutcome::Prepared,
    })?;

    let committed = client
        .wait_for_command(token.command_sequence, TIMEOUT)
        .ok_or("the runtime never finished the model commit")?;
    if let Some(failure) = committed.last_command_failure
        && failure.sequence == token.command_sequence
    {
        return Err(format!("model commit failed: {}", failure.code).into());
    }
    Ok(())
}

fn publish(
    consumer: &RenderConsumer,
    sink: &mut MagneoSink,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut trace_window = Instant::now();
    let mut traced_frames = 0u32;
    loop {
        let Some(frame) = consumer.take_latest() else {
            thread::sleep(POLL_INTERVAL);
            continue;
        };

        if trace_keys() {
            traced_frames += 1;
            if trace_window.elapsed() >= Duration::from_secs(1) {
                let keys: Vec<u32> = frame
                    .snapshot
                    .active_keys
                    .iter()
                    .map(|key| u32::try_from(key.asset_id.index()).unwrap_or(u32::MAX))
                    .collect();
                eprintln!(
                    "catscreen: {traced_frames} frames/s, {} keys {keys:?}",
                    keys.len()
                );
                traced_frames = 0;
                trace_window = Instant::now();
            }
        }

        sink.deliver(&frame);
    }
}

struct Options {
    address: String,
    model: String,
    models: PathBuf,
}

impl Options {
    fn parse() -> Result<Self, Box<dyn std::error::Error>> {
        let mut options = Self {
            address: DEFAULT_ADDRESS.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            models: default_models_root(),
        };

        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            match argument.as_str() {
                "--address" => options.address = next_value(&mut arguments, "--address")?,
                "--model" => options.model = next_value(&mut arguments, "--model")?,
                "--models" => {
                    options.models = PathBuf::from(next_value(&mut arguments, "--models")?)
                }
                "--help" | "-h" => {
                    eprintln!(
                        "usage: catscreen [--address ADDR] [--model ID] [--models DIR]\n\
                         \n\
                         Runs the model headlessly and streams RenderSnapshot frames to a\n\
                         client on ADDR (default {DEFAULT_ADDRESS}). The device reaches it\n\
                         through `adb reverse tcp:7391 tcp:7391`."
                    );
                    std::process::exit(0);
                }
                other => return Err(format!("unknown argument: {other}").into()),
            }
        }

        if !options.models.is_dir() {
            return Err(format!(
                "model directory does not exist: {}",
                options.models.display()
            )
            .into());
        }
        Ok(options)
    }
}

fn next_value(
    arguments: &mut impl Iterator<Item = String>,
    name: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    arguments
        .next()
        .ok_or_else(|| format!("{name} needs a value").into())
}

fn default_models_root() -> PathBuf {
    // The model packages are the desktop product's, so they are read from the
    // upstream checkout beside this repository rather than kept in a second copy
    // that could drift from the one the product ships.
    if let Some(root) = std::env::var_os("CATSCREEN_MODELS") {
        return PathBuf::from(root);
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        let bundled = directory.join("resources/models");
        if bundled.is_dir() {
            return bundled;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kat/resources/models")
}
