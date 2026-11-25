//! Where you are, from a helper command that prints `LAT LON [ACCURACY_M]`.
//!
//! A command-line program can't ask macOS for Location Services permission itself (the
//! prompt needs an app bundle with a usage description), so on macOS the default helper is
//! [CoreLocationCLI](https://github.com/fulldecent/corelocationcli)
//! (`brew install --cask corelocationcli`), which is such an app. Any other command that
//! prints the same (gpsd, a phone, a fixed spot) works through `--location-cmd` or
//! `MAPTERM_LOCATION_CMD`.

use anyhow::{Context, Result, bail};
use std::time::Duration;

/// A position fix: degrees, and how far off it may be in meters, when the helper says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fix {
    /// Latitude, degrees north.
    pub lat: f64,
    /// Longitude, degrees east.
    pub lon: f64,
    /// Horizontal accuracy in meters (the radius it's likely within), if known.
    pub accuracy_m: Option<f64>,
}

impl Fix {
    /// A zoom that shows the fix's uncertainty: close in for a Wi-Fi or GPS fix, wider for a
    /// rough one.
    pub fn zoom(&self) -> f64 {
        match self.accuracy_m {
            Some(m) if m <= 50.0 => 16.0,
            Some(m) if m <= 200.0 => 15.0,
            Some(m) if m <= 1_000.0 => 14.0,
            Some(m) if m <= 5_000.0 => 12.0,
            Some(_) => 10.0,
            None => 15.0,
        }
    }
}

/// How long a helper may take: Location Services can need a few seconds for a first fix.
const TIMEOUT: Duration = Duration::from_secs(20);

/// The command that finds the location: `custom` (`--location-cmd`), else
/// `MAPTERM_LOCATION_CMD`, else CoreLocationCLI on macOS when it's installed.
pub fn command(custom: Option<&str>) -> Option<String> {
    if let Some(c) = custom.map(str::trim).filter(|c| !c.is_empty()) {
        return Some(c.to_string());
    }
    if let Some(c) = std::env::var("MAPTERM_LOCATION_CMD").ok().filter(|c| !c.trim().is_empty()) {
        return Some(c);
    }
    if cfg!(target_os = "macos") && on_path("CoreLocationCLI") {
        return Some("CoreLocationCLI --format '%latitude %longitude %h_accuracy'".to_string());
    }
    None
}

/// Find the location with [`command`]. Blocks while the helper runs (up to 20 s).
pub fn locate(custom: Option<&str>) -> Result<Fix> {
    let Some(cmd) = command(custom) else {
        if cfg!(target_os = "macos") {
            bail!("no location helper: install CoreLocationCLI (brew install --cask corelocationcli), or set --location-cmd");
        }
        bail!("no location helper: set --location-cmd or MAPTERM_LOCATION_CMD to a command that prints LAT LON");
    };
    let out = run(&cmd)?;
    parse(&out).with_context(|| format!("the location command printed no LAT LON: {:?}", out.trim()))
}

/// Read `LAT LON [ACCURACY]` (spaces or commas) from the first line that has them.
pub fn parse(out: &str) -> Option<Fix> {
    out.lines().find_map(|line| {
        let mut nums = line.split(|c: char| c.is_whitespace() || c == ',').filter(|s| !s.is_empty()).map(str::parse::<f64>);
        let lat = nums.next()?.ok()?;
        let lon = nums.next()?.ok()?;
        // CoreLocationCLI reports a negative accuracy when it has none.
        let accuracy_m = nums.next().and_then(Result::ok).filter(|m| m.is_finite() && *m >= 0.0);
        ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some(Fix { lat, lon, accuracy_m })
    })
}

/// Run `cmd` with `sh -c` and return its output, stopping it after [`TIMEOUT`].
fn run(cmd: &str) -> Result<String> {
    use std::process::{Command, Stdio};
    let child = Command::new("sh")
        .args(["-c", cmd])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("running the location command")?;
    let pid = child.id();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });
    let out = match rx.recv_timeout(TIMEOUT) {
        Ok(out) => out.context("running the location command")?,
        Err(_) => {
            #[cfg(unix)]
            // SAFETY: signalling the child this function started; it hasn't been reaped, since
            // the waiting thread hasn't returned.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
            let _ = pid;
            bail!("the location command took more than {} s", TIMEOUT.as_secs());
        }
    };
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        bail!("the location command failed{}", if err.is_empty() { String::new() } else { format!(": {err}") });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Whether `name` is an executable file in a `PATH` directory.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_corelocationcli_output() {
        assert_eq!(parse("38.889484 -77.035279 65\n"), Some(Fix { lat: 38.889484, lon: -77.035279, accuracy_m: Some(65.0) }));
        // The default format: no accuracy.
        assert_eq!(parse("50.943829 6.941043"), Some(Fix { lat: 50.943829, lon: 6.941043, accuracy_m: None }));
        // No accuracy known.
        assert_eq!(parse("1 2 -1").unwrap().accuracy_m, None);
    }

    #[test]
    fn reads_commas_and_skips_noise() {
        assert_eq!(parse("locating...\n38.9, -77.0\n").map(|f| (f.lat, f.lon)), Some((38.9, -77.0)));
        assert_eq!(parse("kCLErrorDomain error 0"), None);
        assert_eq!(parse("120 10"), None, "latitude out of range");
    }

    #[test]
    fn zoom_follows_accuracy() {
        let at = |m| Fix { lat: 0.0, lon: 0.0, accuracy_m: m }.zoom();
        assert_eq!(at(Some(30.0)), 16.0);
        assert_eq!(at(Some(65.0)), 15.0);
        assert_eq!(at(Some(20_000.0)), 10.0);
        assert_eq!(at(None), 15.0);
    }

    #[test]
    fn runs_a_custom_command() {
        let fix = locate(Some("echo 38.8895 -77.0353 10")).unwrap();
        assert_eq!((fix.lat, fix.lon, fix.accuracy_m), (38.8895, -77.0353, Some(10.0)));
        assert!(locate(Some("echo nowhere")).is_err());
        assert!(locate(Some("exit 3")).is_err());
    }
}
