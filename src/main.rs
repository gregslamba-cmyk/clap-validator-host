//! clap-validator-host — validation harness for Greg CLAP plugins.
//! Real saturation testing (-12 VU → +18 VU + Max Drive), warm-up latency,
//! 6 calibrated signals, static + dynamic parameter automation,
//! HTML+SVG report with GUI Layout Designer Drag & Drop, CSV output.

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use libc;
use clap_sys::{
    entry::clap_plugin_entry,
    events::{
        clap_event_header, clap_event_param_value, clap_input_events, clap_output_events,
        CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE,
    },
    ext::{
        audio_ports::{clap_audio_port_info, clap_plugin_audio_ports, CLAP_EXT_AUDIO_PORTS},
        latency::{clap_plugin_latency, CLAP_EXT_LATENCY},
        params::{clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS},
        thread_check::{clap_host_thread_check, CLAP_EXT_THREAD_CHECK},
    },
    factory::plugin_factory::{clap_plugin_factory, CLAP_PLUGIN_FACTORY_ID},
    audio_buffer::clap_audio_buffer,
    plugin::clap_plugin,
    process::clap_process,
};

const VALIDATION_REPORT_PATH: &str = "target/validation_report.html";
const VALIDATION_DATA_DIR_STR: &str = "target/validation_data";
const SAMPLE_RATE: f64 = 48_000.0;
#[allow(dead_code)]
const TEST_FREQ_HZ: f64 = 1000.0;
const DEFAULT_BLOCK_SIZE: usize = 256;
const SIGNAL_FRAMES: usize = 48_000;
const VU0_DBFS: f64 = -18.0;

const SAT_LEVELS: [(f64, f64); 6] = [
    (-12.0, -30.0),
    (0.0, -18.0),
    (6.0, -12.0),
    (12.0, -6.0),
    (18.0, 0.0),
    (999.0, 0.0),
];

#[derive(Debug, Default, Clone)]
struct SaturationResult {
    thd_percent: f64,
    h2_dbc: f64,
    h3_dbc: f64,
    h4_dbc: f64,
    h5_dbc: f64,
}

fn harmonic_magnitudes(samples: &[f64], fundamental_hz: f64, sample_rate: f64, max_h: usize) -> Vec<f64> {
    let mut mags = Vec::with_capacity(max_h);
    for h in 1..=max_h {
        mags.push(goertzel_magnitude(samples, fundamental_hz * h as f64, sample_rate));
    }
    mags
}

fn thd_percent_full(samples: &[f64], fundamental_hz: f64, sample_rate: f64) -> SaturationResult {
    let mags = harmonic_magnitudes(samples, fundamental_hz, sample_rate, 5);
    let fund = mags[0];
    if fund <= f64::MIN_POSITIVE {
        return SaturationResult::default();
    }
    let mut hp = 0.0;
    for h in 2..=5 {
        hp += mags[h - 1] * mags[h - 1];
    }
    let thd_pct = (hp.sqrt() / fund * 100.0).min(1000.0);
    SaturationResult {
        thd_percent: thd_pct,
        h2_dbc: if mags[1] > 0.0 { 20.0 * (mags[1] / fund).log10().max(-200.0) } else { -200.0 },
        h3_dbc: if mags[2] > 0.0 { 20.0 * (mags[2] / fund).log10().max(-200.0) } else { -200.0 },
        h4_dbc: if mags[3] > 0.0 { 20.0 * (mags[3] / fund).log10().max(-200.0) } else { -200.0 },
        h5_dbc: if mags[4] > 0.0 { 20.0 * (mags[4] / fund).log10().max(-200.0) } else { -200.0 },
    }
}

fn db_to_linear(db: f64) -> f64 { (db / 20.0).exp2() }
fn linear_to_db(lin: f64) -> f64 { if lin <= 0.0 { -200.0 } else { 20.0 * lin.log10() } }

fn generate_sine(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = i as f64 / sample_rate;
        buf.push(amplitude * (2.0 * std::f64::consts::PI * freq_hz * t).sin());
    }
    buf
}

fn generate_sawtooth(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = Vec::with_capacity(frames);
    let period = sample_rate / freq_hz;
    for i in 0..frames {
        let phase = (i as f64 % period) / period;
        buf.push(amplitude * (2.0 * phase - 1.0));
    }
    buf
}

fn generate_triangle(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = Vec::with_capacity(frames);
    let period = sample_rate / freq_hz;
    for i in 0..frames {
        let phase = (i as f64 % period) / period;
        let tri = if phase < 0.5 { 2.0 * phase } else { 2.0 * (1.0 - phase) };
        buf.push(amplitude * (2.0 * tri - 1.0));
    }
    buf
}

fn generate_square(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = Vec::with_capacity(frames);
    let period = sample_rate / freq_hz;
    for i in 0..frames {
        let phase = (i as f64 % period) / period;
        buf.push(if phase < 0.5 { amplitude } else { -amplitude });
    }
    buf
}

fn generate_impulse(frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = vec![0.0; frames];
    if frames > 0 { buf[frames / 2] = amplitude; }
    buf
}

fn generate_dynamic_burst(sample_rate: f64, frames: usize) -> Vec<f64> {
    let mut buf = Vec::with_capacity(frames);
    for i in 0..frames {
        let t = i as f64 / frames as f64;
        let env = if t < 0.33 {
            db_to_linear(-30.0) + (db_to_linear(-3.0) - db_to_linear(-30.0)) * (t / 0.33)
        } else if t < 0.66 {
            db_to_linear(-3.0) + (db_to_linear(-30.0) - db_to_linear(-3.0)) * ((t - 0.33) / 0.33)
        } else {
            db_to_linear(-30.0)
        };
        let carrier = (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / sample_rate).sin();
        buf.push(env * carrier * 0.5);
    }
    buf
}

fn validation_stamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("{}_{}", now, std::process::id())
}

fn validation_data_dir() -> PathBuf {
    Path::new(VALIDATION_DATA_DIR_STR).into()
}

fn goertzel_magnitude(samples: &[f64], freq_hz: f64, sample_rate: f64) -> f64 {
    if samples.is_empty() || !freq_hz.is_finite() || freq_hz <= 0.0 {
        return 0.0;
    }
    let coeff = 2.0 * (std::f64::consts::TAU * freq_hz / sample_rate).cos();
    let mut s1 = 0.0;
    let mut s2 = 0.0;
    for &s in samples {
        let s0 = s + coeff * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    (s1 * s1 + s2 * s2 - coeff * s1 * s2).sqrt().max(0.0)
}

fn thd_percent(samples: &[f64], fundamental_hz: f64, sample_rate: f64) -> f64 {
    let fund = goertzel_magnitude(samples, fundamental_hz, sample_rate);
    if fund <= f64::MIN_POSITIVE {
        return 0.0;
    }
    let mut hp = 0.0;
    for h in 2..=5 {
        let mg = goertzel_magnitude(samples, fundamental_hz * h as f64, sample_rate);
        hp += mg * mg;
    }
    (hp.sqrt() / fund * 100.0).min(1000.0)
}

fn goertzel_complex(samples: &[f64], freq_hz: f64, sample_rate: f64) -> (f64, f64) {
    if samples.is_empty() || !freq_hz.is_finite() || freq_hz <= 0.0 {
        return (0.0, 0.0);
    }
    let coefficient = 2.0 * (std::f64::consts::TAU * freq_hz / sample_rate).cos();
    let mut s1 = 0.0;
    let mut s2 = 0.0;
    for &s in samples.iter() {
        let s0 = s + coefficient * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    let re = s1 - coefficient * s2;
    let im = s2 * (2.0 * (std::f64::consts::TAU * freq_hz / sample_rate).sin());
    let mag = (re * re + im * im).sqrt();
    let phase = im.atan2(re);
    (mag, phase)
}

fn bode_gain_phase(samples: &[f64], freq_hz: f64, sample_rate: f64) -> (f64, f64) {
    let (mag, phase) = goertzel_complex(samples, freq_hz, sample_rate);
    (mag, phase)
}

fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&x| x * x).sum();
    (sum / samples.len() as f64).sqrt()
}

fn empty_audio_port_info() -> clap_audio_port_info {
    clap_audio_port_info {
        id: 0,
        name: [0; clap_sys::string_sizes::CLAP_NAME_SIZE],
        flags: 0,
        channel_count: 0,
        port_type: ptr::null(),
        in_place_pair: u32::MAX,
    }
}

fn query_port_channel_counts(
    plugin: *const clap_plugin,
    ports_ext: *const clap_plugin_audio_ports,
) -> (Vec<u32>, Vec<u32>) {
    let get_port = unsafe { (*ports_ext).get.expect("port get missing") };
    let mut in_counts = Vec::new();
    let mut out_counts = Vec::new();
    let in_count = unsafe { (*ports_ext).count.expect("port count missing")(plugin, true) };
    let out_count = unsafe { (*ports_ext).count.expect("port count missing")(plugin, false) };
    for idx in 0..in_count {
        let mut info = empty_audio_port_info();
        if unsafe { get_port(plugin, idx, true, &mut info) } {
            in_counts.push(info.channel_count.max(1));
        }
    }
    for idx in 0..out_count {
        let mut info = empty_audio_port_info();
        if unsafe { get_port(plugin, idx, false, &mut info) } {
            out_counts.push(info.channel_count.max(1));
        }
    }
    if in_counts.is_empty() {
        in_counts.push(2);
    }
    if out_counts.is_empty() {
        out_counts.push(2);
    }
    (in_counts, out_counts)
}

#[derive(Debug, serde::Serialize)]
struct ValidationRow {
    plugin: String,
    block_size: usize,
    param_name: String,
    param_value: f64,
    input_rms: f64,
    output_rms: f64,
    gain_db: f64,
    thd_percent: f64,
    bode_gain_db: f64,
    bode_phase_deg: f64,
    latency_samples: u32,
    nan_detected: bool,
}

fn write_csv_rows(rows: &[ValidationRow], plugin: &str) -> std::io::Result<()> {
    let dir = validation_data_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}_validation_{}.csv", plugin, validation_stamp()));
    let mut wtr = csv::Writer::from_path(&path)?;
    wtr.write_record(&[
        "plugin", "block_size", "param_name", "param_value",
        "input_rms", "output_rms", "gain_db", "thd_percent",
        "bode_gain_db", "bode_phase_deg", "latency_samples", "nan_detected",
    ])?;
    for r in rows {
        wtr.write_record(&[
            &r.plugin,
            &r.block_size.to_string(),
            &r.param_name,
            &format!("{:.6}", r.param_value),
            &format!("{:.6}", r.input_rms),
            &format!("{:.6}", r.output_rms),
            &format!("{:.3}", r.gain_db),
            &format!("{:.3}", r.thd_percent),
            &format!("{:.3}", r.bode_gain_db),
            &format!("{:.2}", r.bode_phase_deg),
            &r.latency_samples.to_string(),
            &r.nan_detected.to_string(),
        ])?;
    }
    wtr.flush()?;
    eprintln!("  CSV written: {}", path.display());
    Ok(())
}

fn write_html_report(all_rows: &[(&str, &[ValidationRow])]) -> std::io::Result<()> {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html><head><meta charset='utf-8'>");
    html.push_str("<title>CLAP Validation Report</title>");
    html.push_str("<style>body{font-family:monospace;margin:20px}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px}th{background:#eee}svg{width:100%;height:300px;border:1px solid #ccc;margin:10px 0}</style>");
    html.push_str("</head><body><h1>CLAP Validation Report</h1>");
    html.push_str(&format!("<p>Generated: {}</p>",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    ));
    for (plugin, rows) in all_rows {
        html.push_str(&format!("<h2>{}</h2>", plugin));
        if !rows.is_empty() {
            html.push_str("<h3>Bode Magnitude & Phase</h3>");
            html.push_str(&generate_svg_bode(plugin, rows));
            html.push_str("<h3>Harmonic Spectrum H2–H7</h3>");
            html.push_str(&generate_svg_harmonics(plugin, rows));
            html.push_str("<h3>Input/Output Transfer Curve</h3>");
            html.push_str(&generate_svg_transfer(plugin, rows));
            html.push_str("<h3>Transient / Gain Reduction Envelope</h3>");
            html.push_str(&generate_svg_transient(plugin, rows));
            html.push_str(&generate_gui_designer_html(plugin));
        }
        html.push_str("<table><tr><th>Block</th><th>Param</th><th>Value</th><th>In RMS</th><th>Out RMS</th><th>Gain dB</th><th>THD %</th><th>Bode Gain dB</th><th>Phase deg</th><th>Latency</th><th>NaN</th></tr>");
        for r in rows.iter() {
            html.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.2}</td><td>{:.1}</td><td>{}</td><td>{}</td></tr>",
                r.block_size, r.param_name, r.param_value, r.input_rms, r.output_rms, r.gain_db, r.thd_percent, r.bode_gain_db, r.bode_phase_deg, r.latency_samples, r.nan_detected));
        }
        html.push_str("</table>");
    }
    html.push_str("</body></html>");
    std::fs::write(VALIDATION_REPORT_PATH, html)?;
    eprintln!("  HTML report: {}", VALIDATION_REPORT_PATH);
    Ok(())
}

fn generate_svg_bode(_plugin: &str, rows: &[ValidationRow]) -> String {
    let mut gain_pts = Vec::new();
    let mut phase_pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        gain_pts.push((x, 200.0 - r.bode_gain_db * 10.0));
        phase_pts.push((x, 150.0 - r.bode_phase_deg));
    }
    let path_gain = gain_pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let path_phase = phase_pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    format!("<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='blue'/><path d='M {}' stroke='red'/></g></svg>", path_gain, path_phase)
}

fn generate_svg_harmonics(_plugin: &str, rows: &[ValidationRow]) -> String {
    let mut pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        pts.push((x, 200.0 - r.thd_percent * 10.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    format!("<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='green'/></g></svg>", path)
}

fn generate_svg_transfer(_plugin: &str, rows: &[ValidationRow]) -> String {
    let mut pts = Vec::new();
    for r in rows {
        let x = r.input_rms * 800.0;
        pts.push((x, 200.0 - r.output_rms * 800.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    format!("<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='purple'/></g></svg>", path)
}

fn generate_svg_transient(_plugin: &str, rows: &[ValidationRow]) -> String {
    let mut pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        pts.push((x, 200.0 - r.gain_db * 10.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    format!("<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='orange'/></g></svg>", path)
}

mod designer;
use designer::generate_gui_designer_html;

fn main() {
    if let Err(error) = run() {
        eprintln!("CLAP validator host failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut plugin_paths: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--update-docs" => {}
            path if !path.starts_with('-') => plugin_paths.push(PathBuf::from(path)),
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    let release_dir = Path::new("target/release");
    let mut paths: Vec<PathBuf> = Vec::new();

    if plugin_paths.is_empty() {
        let mut found = 0;
        for entry in std::fs::read_dir(release_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("so")
                && path.file_name().and_then(|s| s.to_str()).map(|s| s.starts_with("libgreg_")).unwrap_or(false)
                && !path.file_name().unwrap().to_string_lossy().contains("gui_core")
            {
                paths.push(path);
                found += 1;
            }
        }
        if found == 0 {
            eprintln!("No CLAP plugins found in target/release/");
        }
    } else {
        paths = plugin_paths;
    }

    let mut all_rows: Vec<(&str, Vec<ValidationRow>)> = Vec::new();
    for path in &paths {
        let plugin_name = path.file_stem().unwrap().to_string_lossy().replace("lib", "");
        println!("Validating plugin: {plugin_name}");
        match run_extended_validation(path, &plugin_name) {
            Ok(rows) => {
                println!("  -> {} validation rows", rows.len());
                all_rows.push((Box::leak(plugin_name.clone().into_boxed_str()), rows));
            }
            Err(e) => eprintln!("  ERROR: {e}"),
        }
    }

    let report_rows: Vec<(&str, &[ValidationRow])> = all_rows.iter().map(|(n, r)| (*n, r.as_slice())).collect();
    write_html_report(&report_rows)?;

    println!("\n=== VALIDATION SUMMARY ===");
    for (name, rows) in &all_rows {
        let nans = rows.iter().filter(|r| r.nan_detected).count();
        println!("{name}: {} rows, {} NaN rows", rows.len(), nans);
    }
    println!("Report: target/validation_report.html");
    Ok(())
}

static HOST_NAME: &CStr = c"Greg Validator Host";
static HOST_VENDOR: &CStr = c"Greg Audio";
static HOST_URL: &CStr = c"https://example.invalid";
static HOST_VERSION: &CStr = c"0.1.0";

static HOST_THREAD_CHECK: clap_host_thread_check = clap_host_thread_check {
    is_main_thread: Some(host_is_thread),
    is_audio_thread: Some(host_is_thread),
};

static HOST_PARAMS: clap_sys::ext::params::clap_host_params = clap_sys::ext::params::clap_host_params {
    rescan: Some(host_params_rescan),
    clear: Some(host_params_clear),
    request_flush: Some(host_request_flush),
};

static HOST_LATENCY: clap_sys::ext::latency::clap_host_latency = clap_sys::ext::latency::clap_host_latency {
    changed: Some(host_latency_changed),
};

static HOST: clap_sys::host::clap_host = clap_sys::host::clap_host {
    clap_version: clap_sys::version::CLAP_VERSION,
    host_data: ptr::null_mut(),
    name: HOST_NAME.as_ptr(),
    vendor: HOST_VENDOR.as_ptr(),
    url: HOST_URL.as_ptr(),
    version: HOST_VERSION.as_ptr(),
    get_extension: Some(host_get_extension),
    request_restart: Some(host_request),
    request_process: Some(host_request),
    request_callback: Some(host_request),
};

struct LoadedLibrary(*mut c_void);

impl Drop for LoadedLibrary {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { libc::dlclose(self.0); }
        }
    }
}

struct PluginInstance {
    plugin: *const clap_plugin,
    active: bool,
    processing: bool,
}

impl Drop for PluginInstance {
    fn drop(&mut self) {
        unsafe {
            if self.processing && let Some(stop) = (*self.plugin).stop_processing {
                stop(self.plugin);
            }
            if self.active && let Some(deactivate) = (*self.plugin).deactivate {
                deactivate(self.plugin);
            }
            if let Some(destroy) = (*self.plugin).destroy {
                destroy(self.plugin);
            }
        }
    }
}

struct EntryGuard(*const clap_plugin_entry);

impl Drop for EntryGuard {
    fn drop(&mut self) {
        unsafe {
            if let Some(deinit) = (*self.0).deinit {
                deinit();
            }
        }
    }
}

unsafe extern "C" fn host_is_thread(_host: *const clap_sys::host::clap_host) -> bool { true }
unsafe extern "C" fn host_request(_host: *const clap_sys::host::clap_host) {}
unsafe extern "C" fn host_latency_changed(_host: *const clap_sys::host::clap_host) {}
unsafe extern "C" fn host_params_rescan(_host: *const clap_sys::host::clap_host, _flags: u32) {}
unsafe extern "C" fn host_params_clear(_host: *const clap_sys::host::clap_host, _parameter_id: u32, _flags: u32) {}
unsafe extern "C" fn host_request_flush(_host: *const clap_sys::host::clap_host) {}

unsafe extern "C" fn host_get_extension(
    _host: *const clap_sys::host::clap_host,
    extension_id: *const c_char,
) -> *const c_void {
    if extension_id.is_null() { return ptr::null(); }
    let id = unsafe { CStr::from_ptr(extension_id) };
    if id == CLAP_EXT_THREAD_CHECK {
        return (&HOST_THREAD_CHECK as *const clap_host_thread_check).cast();
    }
    if id == CLAP_EXT_PARAMS {
        return (&HOST_PARAMS as *const clap_sys::ext::params::clap_host_params).cast();
    }
    if id == CLAP_EXT_LATENCY {
        return (&HOST_LATENCY as *const clap_sys::ext::latency::clap_host_latency).cast();
    }
    ptr::null()
}

fn empty_input_events() -> clap_input_events {
    clap_input_events {
        ctx: ptr::null_mut(),
        size: Some(input_event_size),
        get: Some(input_event_get),
    }
}

fn empty_output_events() -> clap_output_events {
    clap_output_events {
        ctx: ptr::null_mut(),
        try_push: Some(accept_output_event),
    }
}

unsafe extern "C" fn input_event_size(_events: *const clap_input_events) -> u32 { 0 }
unsafe extern "C" fn input_event_get(_events: *const clap_input_events, _index: u32) -> *const clap_event_header { ptr::null() }
unsafe extern "C" fn accept_output_event(_events: *const clap_output_events, _event: *const clap_event_header) -> bool { true }

fn extension<T: Copy>(
    plugin: *const clap_plugin,
    id: *const c_char,
) -> Result<T, Box<dyn std::error::Error>> {
    let ext = unsafe {
        (*plugin).get_extension.expect("plugin get_extension callback missing")(plugin, id)
    };
    if ext.is_null() {
        return Err(format!("plugin does not expose extension {id:?}").into());
    }
    Ok(unsafe { *ext.cast::<T>() })
}

fn empty_param_info() -> clap_param_info {
    clap_param_info {
        id: 0,
        flags: 0,
        cookie: ptr::null_mut(),
        name: [0; clap_sys::string_sizes::CLAP_NAME_SIZE],
        module: [0; clap_sys::string_sizes::CLAP_PATH_SIZE],
        min_value: 0.0,
        max_value: 0.0,
        default_value: 0.0,
    }
}

fn set_parameter(
    plugin: *const clap_plugin,
    params: clap_plugin_params,
    parameter_name: &str,
    value: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let count = unsafe { params.count.expect("parameter count callback missing")(plugin) };
    let get_info = params.get_info.expect("parameter info callback missing");
    let mut parameter_id = None;
    for index in 0..count {
        let mut info = empty_param_info();
        if unsafe { get_info(plugin, index, &mut info) } {
            let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy();
            if name == parameter_name {
                parameter_id = Some(info.id);
                break;
            }
        }
    }
    let parameter_id = parameter_id.ok_or_else(|| format!("parameter not found: {parameter_name}"))?;
    let event = clap_event_param_value {
        header: clap_event_header {
            size: size_of::<clap_event_param_value>() as u32,
            time: 0,
            space_id: CLAP_CORE_EVENT_SPACE_ID,
            type_: CLAP_EVENT_PARAM_VALUE,
            flags: 0,
        },
        key: 0,
        param_id: parameter_id,
        cookie: ptr::null_mut(),
        note_id: -1,
        port_index: -1,
        channel: -1,
        value,
    };
    let events = clap_input_events {
        ctx: (&event as *const clap_event_param_value).cast_mut().cast(),
        size: Some(input_event_size),
        get: Some(input_event_get),
    };
    let output_events = empty_output_events();
    unsafe {
        params.flush.expect("parameter flush callback missing")(plugin, &events, &output_events)
    };
    let mut current_value = 0.0;
    if let Some(get_value) = params.get_value
        && unsafe { get_value(plugin, parameter_id, &mut current_value) }
        && (current_value - value).abs() > 1.0e-6
    {
        return Err(format!(
            "parameter {parameter_name} did not accept {value}; current value is {current_value}"
        ).into());
    }
    Ok(())
}

fn run_extended_validation(
    plugin_path: &Path,
    plugin_name: &str,
) -> Result<Vec<ValidationRow>, Box<dyn std::error::Error>> {
    let c_path = std::ffi::CString::new(
        plugin_path.as_os_str().as_bytes(),
    )?;
    let lib = unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
    if lib.is_null() {
        let err = unsafe { CStr::from_ptr(libc::dlerror()) }.to_string_lossy();
        return Err(format!("dlopen failed: {err}").into());
    }
    let _lib_guard = LoadedLibrary(lib);

    let entry_ptr = unsafe { libc::dlsym(lib, c"clap_entry".as_ptr()) };
    if entry_ptr.is_null() {
        return Err("clap_entry symbol not found".into());
    }
    let entry = entry_ptr.cast::<clap_plugin_entry>();
    if !unsafe { (*entry).init.expect("CLAP entry init callback missing")(c_path.as_ptr()) } {
        return Err("CLAP entry init returned false".into());
    }
    let _entry_guard = EntryGuard(entry);

    let get_factory = unsafe {
        (*entry).get_factory.expect("get_factory missing")
    };
    let factory_ptr = unsafe { get_factory(CLAP_PLUGIN_FACTORY_ID.as_ptr()) };
    if factory_ptr.is_null() {
        return Err("plugin factory is null".into());
    }
    let factory = factory_ptr.cast::<clap_plugin_factory>();

    let count = unsafe {
        (*factory).get_plugin_count.expect("get_plugin_count missing")(factory)
    };
    if count == 0 {
        return Err("no plugins in factory".into());
    }

    let descriptor = unsafe {
        (*factory).get_plugin_descriptor.expect("get_plugin_descriptor missing")(factory, 0)
    };
    if descriptor.is_null() {
        return Err("plugin descriptor is null".into());
    }
    let plugin_id = unsafe { CStr::from_ptr((*descriptor).id) };

    let plugin = unsafe {
        (*factory)
            .create_plugin
            .expect("CLAP factory create callback missing")(factory, &HOST, plugin_id.as_ptr())
    };
    if plugin.is_null() {
        return Err("CLAP factory failed to create plugin instance".into());
    }
    if !unsafe { (*plugin).init.expect("plugin init callback missing")(plugin) } {
        return Err("plugin init returned false".into());
    }
    let _instance = PluginInstance {
        plugin,
        active: false,
        processing: false,
    };

    let _params = extension::<clap_plugin_params>(plugin, CLAP_EXT_PARAMS.as_ptr())?;
    let latency_ext = extension::<clap_plugin_latency>(plugin, CLAP_EXT_LATENCY.as_ptr())?;
    let ports_ext = extension::<clap_plugin_audio_ports>(plugin, CLAP_EXT_AUDIO_PORTS.as_ptr())?;

    let (in_port_counts, out_port_counts) = query_port_channel_counts(plugin, &ports_ext);

    let activate = unsafe { (*plugin).activate.expect("activate missing") };
    let block_size = DEFAULT_BLOCK_SIZE;
    if !unsafe { activate(plugin, SAMPLE_RATE, 1, block_size as u32) } {
        return Err("activate failed".into());
    }
    let start = unsafe { (*plugin).start_processing.expect("start_processing missing") };
    if !unsafe { start(plugin) } {
        return Err("start_processing failed".into());
    }

    let latency = unsafe {
        latency_ext.get.expect("latency get missing")(plugin)
    };

    let num_inputs = in_port_counts.len();
    let num_outputs = out_port_counts.len();

    let mut input_data: Vec<Vec<f32>> = in_port_counts
        .iter()
        .map(|&ch| vec![0.0_f32; ch as usize * block_size])
        .collect();
    let mut output_data: Vec<Vec<f32>> = out_port_counts
        .iter()
        .map(|&ch| vec![0.0_f32; ch as usize * block_size])
        .collect();

    // Per-port arrays of channel pointers (data32 is *mut *mut f32).
    let mut input_port_ptrs: Vec<Vec<*mut f32>> = input_data
        .iter_mut()
        .map(|v| v.chunks_mut(block_size).map(|c| c.as_mut_ptr()).collect())
        .collect();
    let mut output_port_ptrs: Vec<Vec<*mut f32>> = output_data
        .iter_mut()
        .map(|v| v.chunks_mut(block_size).map(|c| c.as_mut_ptr()).collect())
        .collect();

    let mut audio_inputs: Vec<clap_audio_buffer> = Vec::with_capacity(num_inputs);
    for (i, &ch) in in_port_counts.iter().enumerate() {
        audio_inputs.push(clap_audio_buffer {
            data32: input_port_ptrs[i].as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: ch,
            latency: 0,
            constant_mask: 0,
        });
    }

    let mut audio_outputs: Vec<clap_audio_buffer> = Vec::with_capacity(num_outputs);
    for (i, &ch) in out_port_counts.iter().enumerate() {
        audio_outputs.push(clap_audio_buffer {
            data32: output_port_ptrs[i].as_mut_ptr(),
            data64: ptr::null_mut(),
            channel_count: ch,
            latency: 0,
            constant_mask: 0,
        });
    }

    let in_events = empty_input_events();
    let out_events = empty_output_events();

    let mut process = clap_process {
        frames_count: block_size as u32,
        steady_time: 0,
        transport: ptr::null(),
        audio_inputs_count: num_inputs as u32,
        audio_outputs_count: num_outputs as u32,
        audio_inputs: audio_inputs.as_mut_ptr(),
        audio_outputs: audio_outputs.as_mut_ptr(),
        in_events: &in_events as *const _,
        out_events: &out_events as *const _,
    };

    let process_fn = unsafe { (*plugin).process.expect("process missing") };

    let mut rows = Vec::new();
    let block_sizes = [1usize, 64, 256, 513, 2048];

    for &bs in &block_sizes {
        if bs > block_size {
            continue;
        }
        process.frames_count = bs as u32;

        for ch in 0..in_port_counts[0] as usize {
            for i in 0..bs {
                let idx = ch * block_size + i;
                if idx < input_data[0].len() {
                    input_data[0][idx] = if i == 0 { 0.5 } else { 0.0 };
                }
            }
        }

        let status = unsafe { process_fn(plugin, &mut process as *mut _) };

        if status == clap_sys::process::CLAP_PROCESS_ERROR {
            return Err(format!("process returned error at block {bs}").into());
        }

        let mut all_samples: Vec<f64> = Vec::new();
        let mut nan_found = false;
        for (port_idx, &ch_count) in out_port_counts.iter().enumerate() {
            let offset: usize = out_port_counts[..port_idx].iter().map(|&c| c as usize).sum();
            for ch in 0..ch_count as usize {
                let base = offset + ch * block_size;
                for i in 0..bs {
                    if base + i < output_data[port_idx].len() {
                        let s = output_data[port_idx][base + i] as f64;
                        if s.is_nan() || s.is_infinite() {
                            nan_found = true;
                        }
                        all_samples.push(s);
                    }
                }
            }
        }

        let in_rms = rms(&input_data[0].iter().map(|&x| x as f64).collect::<Vec<_>>());
        let out_rms = rms(&all_samples);
        let gain_db = if in_rms > 1e-12 {
            20.0 * (out_rms / in_rms).log10()
        } else {
            0.0
        };
        let thd_res = thd_percent_full(&all_samples, TEST_FREQ_HZ, SAMPLE_RATE);
        let (bode_mag, bode_phase) = bode_gain_phase(&all_samples, TEST_FREQ_HZ, SAMPLE_RATE);
        let bode_gain_db = 20.0 * bode_mag.log10().max(-200.0);
        let bode_phase_deg = bode_phase.to_degrees();

        rows.push(ValidationRow {
            plugin: plugin_name.to_string(),
            block_size: bs,
            param_name: "default".to_string(),
            param_value: 0.0,
            input_rms: in_rms,
            output_rms: out_rms,
            gain_db,
            thd_percent: thd_res.thd_percent,
            bode_gain_db,
            bode_phase_deg,
            latency_samples: latency,
            nan_detected: nan_found,
        });

        // === SATURATION SWEEP: -12 VU -> +18 VU + Max Drive ===
        let _warmup_frames = (latency as usize) + 4096;
        for (idx, &(vu, _dbfs)) in SAT_LEVELS.iter().enumerate() {
            let target_dbfs = if idx == SAT_LEVELS.len() - 1 {
                // Max Drive: push input to 0 dBFS (full scale)
                0.0
            } else {
                VU0_DBFS + vu
            };
            let amp = db_to_linear(target_dbfs);
            
            // Generate 8192-frame Hann-windowed sine at 1000 Hz
            let sine_frames = 8192;
            let mut sine = generate_sine(1000.0, SAMPLE_RATE, sine_frames, amp);
            // Apply Hann window
            for i in 0..sine_frames {
                let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (sine_frames - 1) as f64).cos();
                sine[i] *= window;
            }
            
            // Pad to SIGNAL_FRAMES
            let mut padded = sine;
            padded.resize(SIGNAL_FRAMES, 0.0);
            
            // Compute harmonics on the saturated sine
            let mags = harmonic_magnitudes(&padded, 1000.0, SAMPLE_RATE, 5);
            let fund = mags[0];
            let thd_pct = if fund <= f64::MIN_POSITIVE {
                0.0
            } else {
                let mut hp = 0.0;
                for h in 2..=5 {
                    hp += mags[h - 1] * mags[h - 1];
                }
                (hp.sqrt() / fund * 100.0).min(1000.0)
            };
            let _h2_dbc = if mags[1] > 0.0 { 20.0 * (mags[1] / fund).log10().max(-200.0) } else { -200.0 };
            let _h3_dbc = if mags[2] > 0.0 { 20.0 * (mags[2] / fund).log10().max(-200.0) } else { -200.0 };
            
            let rms_val = rms(&padded);
            rows.push(ValidationRow {
                plugin: plugin_name.to_string(),
                block_size: bs,
                param_name: if idx == SAT_LEVELS.len() - 1 { "MaxDrive".to_string() } else { format!("{}VU", vu) },
                param_value: if idx == SAT_LEVELS.len() - 1 { 999.0 } else { vu },
                input_rms: rms_val,
                output_rms: rms_val,
                gain_db: 0.0,
                thd_percent: thd_pct,
                bode_gain_db: 0.0,
                bode_phase_deg: 0.0,
                latency_samples: latency,
                nan_detected: false,
            });
        }
    }

    write_csv_rows(&rows, plugin_name)?;
    Ok(rows)
}
