use std::io::IsTerminal;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use indicatif::ProgressStyle;
use tracing_indicatif::IndicatifLayer;
use tracing_indicatif::filter::IndicatifFilter;
use tracing_indicatif::span_ext::IndicatifSpanExt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, fmt::format::FmtSpan, layer::Layer};

pub(crate) fn is_progress_enabled() -> bool {
    match std::env::var("PITHOS_PROGRESS").as_deref() {
        Ok("always") => true,
        Ok("never") => false,
        Ok("auto") => std::io::stderr().is_terminal(),
        Ok(_) => std::io::stderr().is_terminal(),
        Err(_) => std::io::stderr().is_terminal(),
    }
}

pub(crate) fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{span_child_prefix}{spinner:.cyan} {span_name} {wide_msg}")
        .unwrap_or_else(|_| ProgressStyle::default_spinner())
}

pub(crate) fn init_tracing_with_progress(verbose: u8) {
    let crate_name = env!("CARGO_CRATE_NAME");
    let env_filter = if verbose > 0 {
        match verbose {
            1 => EnvFilter::new(format!("{crate_name}=debug")),
            2 => EnvFilter::new(format!("{crate_name}=trace")),
            _ => EnvFilter::new("trace"),
        }
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new(format!("warn,{crate_name}=info")))
    };

    let indicatif_layer = IndicatifLayer::new()
        .with_progress_style(
            ProgressStyle::with_template(
                "{span_child_prefix}{spinner:.cyan} {span_name}{{{span_fields}}} {wide_msg}",
            )
            .unwrap(),
        )
        .with_max_progress_bars(8, None);

    let filter = IndicatifFilter::new(false);

    let subscriber = tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(false)
                .with_span_events(FmtSpan::CLOSE)
                .with_writer(indicatif_layer.get_stderr_writer()),
        )
        .with(indicatif_layer.with_filter(filter));

    let _ = subscriber.try_init();
}

pub(crate) fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

pub(crate) struct CopyProgress {
    pub span: tracing::Span,
    pub files: Arc<AtomicU64>,
    pub bytes: Arc<AtomicU64>,
    pub cloned: Arc<AtomicU64>,
    pub symlinks: Arc<AtomicU64>,
}

impl CopyProgress {
    pub(crate) fn inc_file(&self, bytes: u64, cloned: bool) {
        self.files.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        if cloned {
            self.cloned.fetch_add(1, Ordering::Relaxed);
        }
        self.span.pb_inc(1);
    }

    pub(crate) fn inc_symlink(&self) {
        self.symlinks.fetch_add(1, Ordering::Relaxed);
        self.span.pb_inc(1);
    }
}

fn new_copy_progress(
    span: tracing::Span,
) -> (CopyProgress, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let progress = CopyProgress {
        span: span.clone(),
        files: Arc::new(AtomicU64::new(0)),
        bytes: Arc::new(AtomicU64::new(0)),
        cloned: Arc::new(AtomicU64::new(0)),
        symlinks: Arc::new(AtomicU64::new(0)),
    };
    let done = Arc::new(AtomicBool::new(false));
    let monitor_span = span.clone();
    let monitor_done = Arc::clone(&done);
    let monitor_files = Arc::clone(&progress.files);
    let monitor_bytes = Arc::clone(&progress.bytes);
    let monitor_cloned = Arc::clone(&progress.cloned);
    let monitor_symlinks = Arc::clone(&progress.symlinks);
    let handle = std::thread::spawn(move || {
        while !monitor_done.load(Ordering::Relaxed) {
            let file_count = monitor_files.load(Ordering::Relaxed);
            let byte_count = monitor_bytes.load(Ordering::Relaxed);
            let cloned_count = monitor_cloned.load(Ordering::Relaxed);
            let symlink_count = monitor_symlinks.load(Ordering::Relaxed);
            if file_count > 0 || symlink_count > 0 {
                let message = if cloned_count > 0 {
                    format!(
                        "{} files • {} • {} cloned • {} symlinks",
                        file_count,
                        format_bytes(byte_count),
                        cloned_count,
                        symlink_count
                    )
                } else if symlink_count > 0 {
                    format!(
                        "{} files • {} • {} symlinks",
                        file_count,
                        format_bytes(byte_count),
                        symlink_count
                    )
                } else {
                    format!("{} files • {}", file_count, format_bytes(byte_count))
                };
                monitor_span.pb_set_message(&message);
            }
            std::thread::sleep(Duration::from_millis(80));
        }
    });
    (progress, done, handle)
}

pub(crate) fn with_copy_progress<F>(f: F) -> eyre::Result<crate::sandbox::CopyStats>
where
    F: FnOnce(&CopyProgress) -> eyre::Result<crate::sandbox::CopyStats>,
{
    let span = tracing::info_span!("copying workspace", indicatif.pb_show = true);
    span.pb_set_style(&spinner_style());
    span.pb_set_message("preparing...");
    let _enter = span.enter();
    let (progress, done, handle) = new_copy_progress(span.clone());
    let started = std::time::Instant::now();
    let result = f(&progress);
    if let Ok(stats) = &result {
        let elapsed = started.elapsed();
        let total_bytes = format_bytes(stats.bytes);
        let finish_message = if stats.cloned > 0 {
            format!(
                "copied {} files • {} • {} cloned • {} symlinks in {}ms",
                stats.files,
                total_bytes,
                stats.cloned,
                stats.symlinks,
                elapsed.as_millis()
            )
        } else {
            format!(
                "copied {} files • {} • {} symlinks in {}ms",
                stats.files,
                total_bytes,
                stats.symlinks,
                elapsed.as_millis()
            )
        };
        span.pb_set_message(&finish_message);
    }
    done.store(true, Ordering::Relaxed);
    let _ = handle.join();
    result
}

pub(crate) fn with_apply_progress<F>(f: F) -> eyre::Result<()>
where
    F: FnOnce(&CopyProgress) -> eyre::Result<()>,
{
    let span = tracing::info_span!("applying changes", indicatif.pb_show = true);
    span.pb_set_style(&spinner_style());
    span.pb_set_message("preparing...");
    let _enter = span.enter();
    let (progress, done, handle) = new_copy_progress(span.clone());
    let started = std::time::Instant::now();
    let result = f(&progress);
    if result.is_ok() {
        let file_count = progress.files.load(Ordering::Relaxed);
        let byte_count = progress.bytes.load(Ordering::Relaxed);
        let symlink_count = progress.symlinks.load(Ordering::Relaxed);
        let message = format!(
            "applied {} files • {} • {} symlinks in {}ms",
            file_count,
            format_bytes(byte_count),
            symlink_count,
            started.elapsed().as_millis()
        );
        span.pb_set_message(&message);
    }
    done.store(true, Ordering::Relaxed);
    let _ = handle.join();
    result
}

pub(crate) struct CountProgress {
    span: tracing::Span,
    done: usize,
    total: usize,
}

impl CountProgress {
    pub(crate) fn inc(&mut self) {
        self.done += 1;
        self.span.pb_inc(1);
        let msg = format!("resolving {}/{} hosts...", self.done, self.total);
        self.span.pb_set_message(&msg);
    }
}

pub(crate) fn with_resolve_progress<F>(
    total: usize,
    f: F,
) -> (Vec<std::net::Ipv4Addr>, Vec<std::net::Ipv6Addr>)
where
    F: FnOnce(&mut Option<CountProgress>) -> (Vec<std::net::Ipv4Addr>, Vec<std::net::Ipv6Addr>),
{
    let span = tracing::info_span!(
        "resolving network whitelist",
        indicatif.pb_show = true,
        hosts = total
    );
    span.pb_set_style(&spinner_style());
    span.pb_set_length(total as u64);
    span.pb_set_message(&format!("resolving {} hosts (3s timeout)...", total));
    let _enter = span.enter();
    let started = std::time::Instant::now();
    let mut progress = Some(CountProgress {
        span: span.clone(),
        done: 0,
        total,
    });
    let result = f(&mut progress);
    let elapsed_ms = started.elapsed().as_millis();
    let total_addrs = result.0.len() + result.1.len();
    span.pb_set_message(&format!(
        "resolved {} hosts → {} addresses in {}ms",
        total, total_addrs, elapsed_ms
    ));
    result
}

pub(crate) fn with_worktree_progress<F>(f: F) -> eyre::Result<crate::strategy::CopyStrategy>
where
    F: FnOnce() -> eyre::Result<crate::strategy::CopyStrategy>,
{
    let span = tracing::info_span!("populating workspace (worktree)", indicatif.pb_show = true);
    span.pb_set_style(&spinner_style());
    span.pb_set_message("cloning worktree...");
    let _enter = span.enter();
    let result = f();
    if let Ok(strategy) = &result {
        span.pb_set_message(&format!("workspace ready via {}", strategy.label()));
    }
    result
}
