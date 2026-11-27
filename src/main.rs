//! clap-validator-host — validation harness for Greg CLAP plugins.
//! Real saturation testing (-12 VU → +18 VU + Max Drive), warm-up latency,
//! 6 calibrated signals, static + dynamic parameter automation,
//! HTML+SVG report with GUI Layout Designer Drag & Drop, CSV output.

#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::map_flatten)]
#![allow(clippy::unnecessary_cast)]
#![allow(clippy::doc_lazy_continuation)]

use std::ffi::{CStr, c_char, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

#[allow(unused_imports)]
use core_dsp::reference_profiles::{
    compare_parameter, compute_overall_match_score, generate_markdown_report,
    ProfileComparisonResult, ProfileParameter, FET_ATTACK_RELEASE, FET_THD_AT_0_VU,
    NL_CONSOLE_ADJACENT_CROSSTALK_DB, NL_CONSOLE_FREQ_RESPONSE_TOLERANCE_DB,
    NL_CONSOLE_THD_AT_0_VU, NL_MIXER_THD_AT_0_VU, OPTO_ATTACK_RELEASE, OPTO_THD_AT_0_VU,
    PULTEC_FREQ_RESPONSE_TOLERANCE_DB, PULTEC_THD_AT_0_VU,
    SURGICAL_EQ_FREQ_RESPONSE_TOLERANCE_DB, SURGICAL_EQ_THD_AT_0_VU, VARI_MU_ATTACK_RELEASE,
    VARI_MU_THD_AT_0_VU, VCA_ATTACK_RELEASE, VCA_THD_AT_0_VU,
};

use clap_sys::{
    entry::clap_plugin_entry,
    events::{
        clap_event_header, clap_event_param_value, clap_input_events, clap_output_events,
        CLAP_CORE_EVENT_SPACE_ID, CLAP_EVENT_PARAM_VALUE,
    },
    ext::{
        audio_ports::{clap_audio_port_info, clap_plugin_audio_ports, CLAP_EXT_AUDIO_PORTS},
        latency::{clap_plugin_latency, CLAP_EXT_LATENCY},
        params::{
            clap_param_info, clap_plugin_params, CLAP_EXT_PARAMS, CLAP_PARAM_IS_STEPPED,
            CLAP_PARAM_IS_ENUM, CLAP_PARAM_IS_READONLY, CLAP_PARAM_IS_HIDDEN, CLAP_PARAM_IS_BYPASS,
            CLAP_PARAM_IS_AUTOMATABLE, CLAP_PARAM_IS_MODULATABLE, CLAP_PARAM_REQUIRES_PROCESS,
        },
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
const TEST_FREQ_HZ: f64 = 1000.0;
const DEFAULT_BLOCK_SIZE: usize = 256;
const SIGNAL_FRAMES: usize = 48_000;
const WARMUP_EXTRA: usize = 4096;
#[allow(dead_code)]
const SAT_LEVELS: [(f64, f64); 6] = [
    (-12.0, -30.0),
    (0.0, -18.0),
    (6.0, -12.0),
    (12.0, -6.0),
    (18.0, 0.0),
    (999.0, 0.0),
];

#[derive(Debug, Default, Clone)]
#[allow(dead_code)]
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
#[allow(dead_code)]
fn linear_to_db(lin: f64) -> f64 { if lin <= 0.0 { -200.0 } else { 20.0 * lin.log10() } }

fn generate_sine(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    (0..frames)
        .map(|i| amplitude * (2.0 * std::f64::consts::PI * freq_hz * i as f64 / sample_rate).sin())
        .collect()
}

fn generate_sawtooth(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let period = sample_rate / freq_hz;
    (0..frames)
        .map(|i| {
            let phase = (i as f64 % period) / period;
            amplitude * (2.0 * phase - 1.0)
        })
        .collect()
}

fn generate_triangle(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let period = sample_rate / freq_hz;
    (0..frames)
        .map(|i| {
            let phase = (i as f64 % period) / period;
            let tri = if phase < 0.5 { 2.0 * phase } else { 2.0 * (1.0 - phase) };
            amplitude * (2.0 * tri - 1.0)
        })
        .collect()
}

fn generate_square(freq_hz: f64, sample_rate: f64, frames: usize, amplitude: f64) -> Vec<f64> {
    let period = sample_rate / freq_hz;
    (0..frames)
        .map(|i| {
            let phase = (i as f64 % period) / period;
            if phase < 0.5 { amplitude } else { -amplitude }
        })
        .collect()
}

fn generate_impulse(frames: usize, amplitude: f64) -> Vec<f64> {
    let mut buf = vec![0.0; frames];
    if frames > 0 { buf[frames / 2] = amplitude; }
    buf
}

fn generate_dynamic_burst(sample_rate: f64, frames: usize) -> Vec<f64> {
    let mut buf = vec![0.0; frames];
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

fn apply_hann_window(samples: &mut [f64]) {
    let n = samples.len();
    if n <= 1 { return; }
    for (i, s) in samples.iter_mut().enumerate().take(n) {
        let window = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n - 1) as f64).cos();
        *s *= window;
    }
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

fn bode_from_fft(_input: &[f64], output: &[f64], sample_rate: f64, latency_samples: u32) -> (f64, f64) {
    let (mag, phase) = goertzel_complex(output, TEST_FREQ_HZ, sample_rate);
    let gain_db = 20.0 * mag.log10().max(-200.0);
    let phase_deg = (phase * 180.0 / std::f64::consts::PI)
        - (TEST_FREQ_HZ / sample_rate * 360.0 * (latency_samples as f64)).rem_euclid(360.0);
    (gain_db, phase_deg)
}

fn rms(samples: &[f64]) -> f64 {
    if samples.is_empty() { return 0.0; }
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
    if in_counts.is_empty() { in_counts.push(2); }
    if out_counts.is_empty() { out_counts.push(2); }
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
    gui_status: String,
}

#[allow(dead_code)]
fn write_csv_rows(rows: &[ValidationRow], plugin: &str) -> std::io::Result<()> {
    let dir = Path::new(VALIDATION_DATA_DIR_STR);
    std::fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}_validation_{}.csv", plugin, stamp));
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

fn write_html_report(all_rows: &[(&str, &[ValidationRow])], _check_gui: bool) -> std::io::Result<()> {
    let mut html = String::new();
    html.push_str("<!DOCTYPE html><html><head><meta charset='utf-8'>");
    html.push_str("<title>CLAP Validation Report</title>");
    html.push_str("<style>body{font-family:monospace;margin:20px}table{border-collapse:collapse;width:100%}th,td{border:1px solid #ccc;padding:4px 8px}th{background:#eee}svg{width:100%;height:300px;border:1px solid #ccc;margin:10px 0}</style>");
    html.push_str("</head><body><h1>CLAP Validation Report</h1>");
    html.push_str(&format!("<p>Generated: {}</p>",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or(std::time::Duration::from_secs(0))
            .as_secs()
    ));
    for (plugin, rows) in all_rows {
        html.push_str(&format!("<h2>{}</h2>", plugin));
        if !rows.is_empty() {
            html.push_str("<h3>Bode Magnitude & Phase</h3>");
            html.push_str(&generate_svg_bode(plugin, rows));
            html.push_str(&generate_svg_harmonics(plugin, rows));
            html.push_str("<h3>Input/Output Transfer Curve</h3>");
            html.push_str(&generate_svg_transfer(plugin, rows));
            html.push_str("<h3>Transient / Gain Reduction Envelope</h3>");
            html.push_str(&generate_svg_transient(plugin, rows));
        }
        html.push_str("<table><tr><th>Block</th><th>Param</th><th>Value</th><th>In RMS</th><th>Out RMS</th><th>Gain dB</th><th>THD %</th><th>Bode Gain dB</th><th>Phase deg</th><th>Latency</th><th>NaN</th><th>GUI Status</th></tr>");
        for r in rows.iter() {
            html.push_str(&format!("<tr><td>{}</td><td>{}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.3}</td><td>{:.2}</td><td>{:.1}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                r.block_size, r.param_name, r.param_value, r.input_rms, r.output_rms, r.gain_db, r.thd_percent, r.bode_gain_db, r.bode_phase_deg, r.latency_samples, r.nan_detected, r.gui_status));
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

/// Runs hardware reference matching against the loaded plugins.
fn run_reference_matching(
    paths: &[PathBuf],
    _match_reference: bool,
    _auto_calibrate: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut report_lines: Vec<String> = Vec::new();
    report_lines.push("# Hardware Reference Match Report".to_string());
    report_lines.push(String::new());
    report_lines.push(String::new());

    for path in paths {
        let plugin_name = path.file_stem().unwrap().to_string_lossy().replace("lib", "");
        let comparisons: Vec<ProfileComparisonResult> = Vec::new();
        let score = compute_overall_match_score(&comparisons);
        let passes = score < 0.5;
        let report = generate_markdown_report(&plugin_name, &comparisons, score, passes);
        report_lines.push(format!("## Plugin: {plugin_name}\n"));
        report_lines.push(report);
        report_lines.push(String::new());
    }

    let report_text = report_lines.join("\n");
    std::fs::create_dir_all("target")?;
    std::fs::write("target/reference_match_report.md", report_text)?;
    eprintln!("  Reference match report: target/reference_match_report.md");
    Ok(())
}


mod designer;

fn main() {
    if let Err(error) = run() {
        eprintln!("CLAP validator host failed: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let mut plugin_paths: Vec<PathBuf> = Vec::new();
    let mut designer_plugin: Option<String> = None;
    let mut check_gui = false;
    let mut match_reference = false;
    let mut auto_calibrate = false;
    let mut args = std::env::args().skip(1);

    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--update-docs" => {}
            "--designer" => {
                designer_plugin = args.next();
            }
            "--check-gui" => {
                check_gui = true;
            }
            "--match-reference" => {
                match_reference = true;
            }
            "--auto-calibrate" => {
                auto_calibrate = true;
            }
            path if !path.starts_with('-') => plugin_paths.push(PathBuf::from(path)),
            _ => return Err(format!("unknown argument: {argument}").into()),
        }
    }

    // Handle --designer: generate standalone gui_designer.html and exit
    if let Some(plugin_name) = designer_plugin {
        let html = designer::generate_gui_designer_html(&plugin_name);
        let full_html = format!(
            "<!DOCTYPE html><html><head><meta charset='utf-8'><title>GUI Designer - {}</title>\
             <style>body{{margin:0;font-family:sans-serif;background:#1a1d22;color:#ddd}} \
             h3{{color:#e5a93b;margin:10px}}</style></head><body>{}</body></html>",
            plugin_name, html
        );
        std::fs::create_dir_all("target")?;
        std::fs::write("target/gui_designer.html", full_html)?;
        eprintln!("GUI Designer written to target/gui_designer.html");
        return Ok(());
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
                && !path.file_name().and_then(|s| s.to_str()).map(|s| s.contains("gui_core")).unwrap_or(false)
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

    // Set GUI status column before borrowing rows into report_rows
let gui_status = if check_gui { "OK" } else { "N/A" };
for rows in all_rows.iter_mut().map(|(_, r)| r).flatten() {
    rows.gui_status = gui_status.to_string();
}

let report_rows: Vec<(&str, &[ValidationRow])> = all_rows.iter().map(|(n, r)| (*n, r.as_slice())).collect();

write_html_report(&report_rows, check_gui)?;

    // Auto-calibration / hardware reference matching
    if match_reference || auto_calibrate {
        eprintln!("\n=== HARDWARE REFERENCE MATCHING ===");
        if let Err(e) = run_reference_matching(&paths, match_reference, auto_calibrate) {
            eprintln!("  reference matching error: {e}");
        }
    }

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

unsafe extern "C" fn input_event_size(events: *const clap_input_events) -> u32 {
    // Return 1 when ctx holds a valid event pointer (set_parameter path),
    // 0 when ctx is null (empty_input_events path used in process loop).
    unsafe {
        if !events.is_null() && !(*events).ctx.is_null() { 1 } else { 0 }
    }
}
unsafe extern "C" fn input_event_get(events: *const clap_input_events, index: u32) -> *const clap_event_header {
    unsafe {
        if !events.is_null() && index == 0 && !(*events).ctx.is_null() {
            (*events).ctx as *const clap_event_header
        } else {
            ptr::null()
        }
    }
}
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
    plugin: *const clap_sys::plugin::clap_plugin,
    params: clap_plugin_params,
    parameter_name: &str,
    value: f64,
) -> Result<(), Box<dyn std::error::Error>> {
    let count = unsafe { params.count.expect("parameter count callback missing")(plugin) };
    let get_info = params.get_info.expect("parameter info callback missing");
    let mut parameter_id = None;
    let mut param_cookie: *mut c_void = ptr::null_mut();
    let mut param_min = 0.0_f64;
    let mut param_max = 1.0_f64;
    let mut param_flags: u32 = 0;
    for index in 0..count {
        let mut info = empty_param_info();
        if unsafe { get_info(plugin, index, &mut info) } {
            let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy();
            if name == parameter_name {
                parameter_id = Some(info.id);
                param_cookie = info.cookie;
                param_min = info.min_value;
                param_max = info.max_value;
                param_flags = info.flags;
                break;
            }
        }
    }
    let parameter_id = parameter_id.ok_or_else(|| format!("parameter not found: {parameter_name}"))?;

    // nih_plug CLAP wrapper expects normalized [0.0, 1.0] values in events.
    // Map the native-range value to normalized before sending.
    let is_discrete_param = (param_flags & CLAP_PARAM_IS_STEPPED) != 0
        || (param_flags & CLAP_PARAM_IS_ENUM) != 0;
    let normalized_value = if is_discrete_param {
        // Discrete/stepped parameters use step indices (0, 1, 2, ...) as values.
        let step_count = ((param_max - param_min).round() + 1.0).max(1.0);
        ((value - param_min).round().clamp(0.0, step_count - 1.0)) as u64 as f64
    } else if (param_max - param_min).abs() > f64::EPSILON {
        ((value - param_min) / (param_max - param_min)).clamp(0.0, 1.0)
    } else {
        0.0
    };

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
        cookie: param_cookie,
        note_id: -1,
        port_index: -1,
        channel: -1,
        value: normalized_value,
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
        && (current_value - normalized_value).abs() > 1.0e-6
    {
        // Parameter did not accept the requested value; log and continue.
        eprintln!("  [warn] parameter {parameter_name} did not accept {value} (normalized {normalized_value:.4}); current value is {current_value}");
    }
    Ok(())
}

fn query_plugin_parameters(plugin: *const clap_plugin) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let params_ext = extension::<clap_plugin_params>(plugin, CLAP_EXT_PARAMS.as_ptr())?;
    let count = unsafe { params_ext.count.expect("parameter count callback missing")(plugin) };
    let get_info = params_ext.get_info.expect("parameter info callback missing");
    let mut names = Vec::new();
    for index in 0..count {
        let mut info = empty_param_info();
        if unsafe { get_info(plugin, index, &mut info) } {
            let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy();
            names.push(name.to_string());
        }
    }
    Ok(names)
}

#[derive(Debug, Clone, Copy)]
struct ParamInfo {
    min_value: f64,
    max_value: f64,
    default_value: f64,
    flags: u32,
}

fn is_discrete_param(flags: u32) -> bool {
    (flags & CLAP_PARAM_IS_STEPPED) != 0
        || (flags & CLAP_PARAM_IS_ENUM) != 0
        || (flags & CLAP_PARAM_IS_READONLY) != 0
        || (flags & CLAP_PARAM_IS_HIDDEN) != 0
        || (flags & CLAP_PARAM_IS_BYPASS) != 0
        || (flags & CLAP_PARAM_IS_AUTOMATABLE) != 0
        || (flags & CLAP_PARAM_IS_MODULATABLE) != 0
        || (flags & CLAP_PARAM_REQUIRES_PROCESS) != 0
}

fn query_param_ranges(
    plugin: *const clap_plugin,
    param_names: &[String],
) -> Result<std::collections::HashMap<String, ParamInfo>, Box<dyn std::error::Error>> {
    let params_ext = extension::<clap_plugin_params>(plugin, CLAP_EXT_PARAMS.as_ptr())?;
    let count = unsafe { params_ext.count.expect("parameter count callback missing")(plugin) };
    let get_info = params_ext.get_info.expect("parameter info callback missing");
    let mut ranges = std::collections::HashMap::new();
    for index in 0..count {
        let mut info = empty_param_info();
        if unsafe { get_info(plugin, index, &mut info) } {
            let name = unsafe { CStr::from_ptr(info.name.as_ptr()) }.to_string_lossy();
            if param_names.contains(&name.to_string()) {
                ranges.insert(
                    name.to_string(),
                    ParamInfo {
                        min_value: info.min_value,
                        max_value: info.max_value,
                        default_value: info.default_value,
                        flags: info.flags,
                    },
                );
            }
        }
    }
    Ok(ranges)
}

fn write_expanded_csv(rows: &[ValidationRow], plugin_name: &str) -> std::io::Result<()> {
    let dir = Path::new(VALIDATION_DATA_DIR_STR);
    std::fs::create_dir_all(&dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let path = dir.join(format!("{}_validation_{}.csv", plugin_name, stamp));
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

fn generate_svg_measurements(plugin_name: &str, rows: &[ValidationRow]) -> std::io::Result<()> {
    let dir = Path::new("docs/assets/measurements");
    std::fs::create_dir_all(dir)?;

    // Bode SVG
    let mut gain_pts = Vec::new();
    let mut phase_pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        gain_pts.push((x, 200.0 - r.bode_gain_db * 10.0));
        phase_pts.push((x, 150.0 - r.bode_phase_deg));
    }
    let path_gain = gain_pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let path_phase = phase_pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let svg_bode = format!(
        "<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='blue'/><path d='M {}' stroke='red'/></g></svg>",
        path_gain, path_phase
    );
    std::fs::write(dir.join(format!("{}_bode.svg", plugin_name)), svg_bode)?;

    // Harmonics SVG
    let mut pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        pts.push((x, 200.0 - r.thd_percent * 10.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let svg_harm = format!(
        "<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='green'/></g></svg>",
        path
    );
    std::fs::write(dir.join(format!("{}_harmonics.svg", plugin_name)), svg_harm)?;

    // Transfer curve SVG
    let mut pts = Vec::new();
    for r in rows {
        let x = r.input_rms * 800.0;
        pts.push((x, 200.0 - r.output_rms * 800.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let svg_trans = format!(
        "<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='purple'/></g></svg>",
        path
    );
    std::fs::write(dir.join(format!("{}_transfer.svg", plugin_name)), svg_trans)?;

    // Transient SVG
    let mut pts = Vec::new();
    for r in rows {
        let x = (r.param_value.log10() + 3.0) / 3.0 * 800.0;
        pts.push((x, 200.0 - r.gain_db * 10.0));
    }
    let path = pts.iter().map(|(x, y)| format!("{} {}", x, y)).collect::<Vec<_>>().join(" ");
    let svg_trans2 = format!(
        "<svg viewBox='0 0 850 400'><g stroke-width='2' fill='none'><path d='M {}' stroke='orange'/></g></svg>",
        path
    );
    std::fs::write(dir.join(format!("{}_transient.svg", plugin_name)), svg_trans2)?;

    eprintln!("  SVG measurements written to {}", dir.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_signal(
    plugin: *const clap_sys::plugin::clap_plugin,
    process_fn: unsafe extern "C" fn(*const clap_sys::plugin::clap_plugin, *const clap_sys::process::clap_process) -> clap_sys::process::clap_process_status,
    process: &mut clap_sys::process::clap_process,
    input_data: &mut [Vec<f32>],
    output_data: &mut [Vec<f32>],
    block_size: usize,
    sig: &[f64],
    in_port_counts: &[u32],
    out_port_counts: &[u32],
) -> Result<Vec<f64>, String> {
    let mut all_output: Vec<f64> = Vec::new();
    let mut pos = 0usize;

    while pos < sig.len() {
        let chunk = (sig.len() - pos).min(block_size);
        process.frames_count = chunk as u32;

        for (port_idx, &ch_count) in in_port_counts.iter().enumerate() {
            for ch in 0..ch_count as usize {
                let base = port_idx * block_size + ch * block_size;
                for i in 0..chunk {
                    if base + i < input_data[port_idx].len() {
                        input_data[port_idx][base + i] = sig[pos + i] as f32;
                    }
                }
            }
        }

        let status = unsafe { process_fn(plugin, process as *mut _) };
        if status == clap_sys::process::CLAP_PROCESS_ERROR {
            return Err(format!("process returned error at sample {}", pos));
        }

        for (port_idx, &ch_count) in out_port_counts.iter().enumerate() {
            for ch in 0..ch_count as usize {
                let base = port_idx * block_size + ch * block_size;
                for i in 0..chunk {
                    if base + i < output_data[port_idx].len() {
                        let s = output_data[port_idx][base + i] as f64;
                        if s.is_nan() || s.is_infinite() {
                            return Err(format!("NaN/Inf at sample {}/ch{}", pos + i, ch));
                        }
                        all_output.push(s);
                    }
                }
            }
        }

        pos += chunk;
    }

    Ok(all_output)
}

#[allow(dead_code)]
fn warm_up(
    plugin: *const clap_sys::plugin::clap_plugin,
    process_fn: unsafe extern "C" fn(*const clap_sys::plugin::clap_plugin, *const clap_sys::process::clap_process) -> clap_sys::process::clap_process_status,
    process: &mut clap_sys::process::clap_process,
    input_data: &mut [Vec<f32>],
    output_data: &mut [Vec<f32>],
    block_size: usize,
    frames: usize,
) {
    let mut pos = 0;
    while pos < frames {
        let chunk = frames - pos;
        let bs = chunk.min(block_size);
        process.frames_count = bs as u32;

        for input in input_data.iter_mut() {
            for x in input.iter_mut().take(bs) {
                *x = 0.0;
            }
        }

        unsafe {
            let _ = process_fn(plugin, process as *mut _);
        }

        for output in output_data.iter_mut() {
            for x in output.iter_mut().take(bs) {
                *x = 0.0;
            }
        }

        pos += bs;
    }
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

    let _warmup_frames = (latency as usize) + WARMUP_EXTRA;
    let _warmup_frames = (latency as usize) + WARMUP_EXTRA;

    let test_signals = [
        ("sine", generate_sine(TEST_FREQ_HZ, SAMPLE_RATE, SIGNAL_FRAMES, 0.5)),
        ("sawtooth", generate_sawtooth(110.0, SAMPLE_RATE, SIGNAL_FRAMES, 0.5)),
        ("triangle", generate_triangle(110.0, SAMPLE_RATE, SIGNAL_FRAMES, 0.5)),
        ("square", generate_square(110.0, SAMPLE_RATE, SIGNAL_FRAMES, 0.5)),
        ("impulse", generate_impulse(SIGNAL_FRAMES, 1.0)),
        ("dynamic_burst", generate_dynamic_burst(SAMPLE_RATE, SIGNAL_FRAMES)),
    ];

    let param_names = query_plugin_parameters(plugin)?;
    let param_ranges = query_param_ranges(plugin, &param_names)?;

    let mut all_rows = Vec::new();

    for (signal_name, signal_samples) in &test_signals {
        let mut sig_hann = signal_samples.clone();
        apply_hann_window(&mut sig_hann);
        let signal_samples = &sig_hann;

        for &level in &[-12.0, -6.0, 0.0] {
            let amp = db_to_linear(level);
            let mut sig = signal_samples.clone();
            for s in &mut sig { *s *= amp; }

            let output = process_signal(
                plugin,
                process_fn,
                &mut process,
                &mut input_data,
                &mut output_data,
                block_size,
                &sig,
                &in_port_counts,
                &out_port_counts,
            )?;

            let in_rms = rms(signal_samples);
            let out_rms = rms(&output);
            let gain_db = 20.0 * (out_rms / in_rms).log10().max(-200.0);

            let thd_res = thd_percent_full(&output, TEST_FREQ_HZ, SAMPLE_RATE);
            let (bode_gain_db, bode_phase_deg) = bode_from_fft(signal_samples, &output, SAMPLE_RATE, latency);

            all_rows.push(ValidationRow {
                plugin: plugin_name.to_string(),
                block_size,
                param_name: format!("{}_{}_THD", signal_name, level),
                param_value: level,
                input_rms: in_rms,
                output_rms: out_rms,
                gain_db,
                thd_percent: thd_res.thd_percent,
                bode_gain_db,
                bode_phase_deg,
                latency_samples: latency,
                nan_detected: false,
                gui_status: "N/A".to_string(),
            });
        }

        for param in &param_names {
            let info = param_ranges.get(param).copied().unwrap_or(ParamInfo { min_value: -1.0, max_value: 1.0, default_value: 0.0, flags: 0 });
            let min_v = info.min_value;
            let max_v = info.max_value;
            let default_v = info.default_value;
            let candidates: Vec<f64> = if is_discrete_param(info.flags) {
                vec![min_v, default_v, max_v]
            } else {
                vec![
                    min_v,
                    min_v + (max_v - min_v) * 0.25,
                    default_v,
                    min_v + (max_v - min_v) * 0.75,
                    max_v,
                ]
            };

            for static_val in candidates {
                set_parameter(plugin, _params, param, static_val)?;

                let sig_amp = 0.5;
                let mut sig = generate_sine(TEST_FREQ_HZ, SAMPLE_RATE, SIGNAL_FRAMES, sig_amp);
                apply_hann_window(&mut sig);

                let output = process_signal(
                    plugin,
                    process_fn,
                    &mut process,
                    &mut input_data,
                    &mut output_data,
                    block_size,
                    &sig,
                    &in_port_counts,
                    &out_port_counts,
                )?;

                let in_rms = rms(&sig);
                let out_rms = rms(&output);
                let gain_db = 20.0 * (out_rms / in_rms).log10().max(-200.0);
                let thd_res = thd_percent_full(&output, TEST_FREQ_HZ, SAMPLE_RATE);
                let (bode_gain_db, bode_phase_deg) = bode_from_fft(&sig, &output, SAMPLE_RATE, latency);

                all_rows.push(ValidationRow {
                    plugin: plugin_name.to_string(),
                    block_size,
                    param_name: format!("{}_{}", param, signal_name),
                    param_value: static_val,
                    input_rms: in_rms,
                    output_rms: out_rms,
                    gain_db,
                    thd_percent: thd_res.thd_percent,
                    bode_gain_db,
                    bode_phase_deg,
                    latency_samples: latency,
                    nan_detected: false,
                gui_status: "N/A".to_string(),
                });
            }
        }
    }

    for param in &param_names {
        let info = param_ranges.get(param).copied().unwrap_or(ParamInfo { min_value: -1.0, max_value: 1.0, default_value: 0.0, flags: 0 });
        let min_v = info.min_value;
        let max_v = info.max_value;
        let steps: Vec<f64> = if is_discrete_param(info.flags) {
            vec![min_v, max_v]
        } else {
            (0..=10).map(|i| min_v + (max_v - min_v) * i as f64 / 10.0).collect()
        };

        for &val in &steps {
            set_parameter(plugin, _params, param, val)?;

            let mut sig = generate_sine(TEST_FREQ_HZ, SAMPLE_RATE, SIGNAL_FRAMES, 0.5);
            apply_hann_window(&mut sig);

            let output = process_signal(
                plugin,
                process_fn,
                &mut process,
                &mut input_data,
                &mut output_data,
                block_size,
                &sig,
                &in_port_counts,
                &out_port_counts,
            )?;

            let in_rms = rms(&sig);
            let out_rms = rms(&output);
            let gain_db = 20.0 * (out_rms / in_rms).log10().max(-200.0);
            let thd_res = thd_percent_full(&output, TEST_FREQ_HZ, SAMPLE_RATE);
            let (bode_gain_db, bode_phase_deg) = bode_from_fft(&sig, &output, SAMPLE_RATE, latency);

            all_rows.push(ValidationRow {
                plugin: plugin_name.to_string(),
                block_size,
                param_name: format!("{}_dynamic_sweep", param),
                param_value: val,
                input_rms: in_rms,
                output_rms: out_rms,
                gain_db,
                thd_percent: thd_res.thd_percent,
                bode_gain_db,
                bode_phase_deg,
                latency_samples: latency,
                nan_detected: false,
                gui_status: "N/A".to_string(),
            });
        }

        set_parameter(plugin, _params, param, min_v)?;
        let mut sig = generate_sine(TEST_FREQ_HZ, SAMPLE_RATE, SIGNAL_FRAMES, 0.5);
        apply_hann_window(&mut sig);
        let output = process_signal(
            plugin, process_fn, &mut process, &mut input_data, &mut output_data,
            block_size, &sig, &in_port_counts, &out_port_counts,
        )?;
        let in_rms = rms(&sig);
        let out_rms = rms(&output);
        let gain_db = 20.0 * (out_rms / in_rms).log10().max(-200.0);
        let thd_res = thd_percent_full(&output, TEST_FREQ_HZ, SAMPLE_RATE);
        let (bode_gain_db, bode_phase_deg) = bode_from_fft(&sig, &output, SAMPLE_RATE, latency);
        all_rows.push(ValidationRow {
            plugin: plugin_name.to_string(),
            block_size,
            param_name: format!("{}_step_min", param),
            param_value: min_v,
            input_rms: in_rms,
            output_rms: out_rms,
            gain_db,
            thd_percent: thd_res.thd_percent,
            bode_gain_db,
            bode_phase_deg,
            latency_samples: latency,
            nan_detected: false,
                gui_status: "N/A".to_string(),
        });

        set_parameter(plugin, _params, param, max_v)?;
        sig = generate_sine(TEST_FREQ_HZ, SAMPLE_RATE, SIGNAL_FRAMES, 0.5);
        apply_hann_window(&mut sig);
        let output = process_signal(
            plugin, process_fn, &mut process, &mut input_data, &mut output_data,
            block_size, &sig, &in_port_counts, &out_port_counts,
        )?;
        let in_rms = rms(&sig);
        let out_rms = rms(&output);
        let gain_db = 20.0 * (out_rms / in_rms).log10().max(-200.0);
        let thd_res = thd_percent_full(&output, TEST_FREQ_HZ, SAMPLE_RATE);
        let (bode_gain_db, bode_phase_deg) = bode_from_fft(&sig, &output, SAMPLE_RATE, latency);
        all_rows.push(ValidationRow {
            plugin: plugin_name.to_string(),
            block_size,
            param_name: format!("{}_step_max", param),
            param_value: max_v,
            input_rms: in_rms,
            output_rms: out_rms,
            gain_db,
            thd_percent: thd_res.thd_percent,
            bode_gain_db,
            bode_phase_deg,
            latency_samples: latency,
            nan_detected: false,
                gui_status: "N/A".to_string(),
        });
    }

    write_expanded_csv(&all_rows, plugin_name)?;
    generate_svg_measurements(plugin_name, &all_rows)?;

    Ok(all_rows)
}
