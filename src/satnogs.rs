//! Satellites' radio transmitters from the [SatNOGS DB](https://db.satnogs.org) (Libre Space
//! Foundation): what each satellite transmits on (downlink frequency, mode, baud), and for
//! transceivers and transponders what it listens on. The data is CC BY-SA 4.0: show
//! "SatNOGS DB" wherever it's shown.
//!
//! The whole list is one request (about 4 MB, a list the server keeps cached), kept a day
//! in `~/.cache/mapterm/satnogs`; after an error the old copy is used and nothing is asked
//! again for 2 hours.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde_json::Value;

const URL: &str = "https://db.satnogs.org/api/transmitters/?format=json";

/// One transmitter, transceiver or transponder on a satellite.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Transmitter {
    /// The satellite's NORAD catalog number.
    pub norad: u64,
    /// What it is, in SatNOGS's words ("Mode V/V FM", "HRPT Downlink", "Telemetry").
    pub description: String,
    /// Transmitter, Transceiver or Transponder.
    pub kind: String,
    /// Downlink, Hz: a frequency, or a band (`downlink_high`) for a transponder.
    pub downlink_low: Option<u64>,
    /// The top of a transponder's downlink band, Hz.
    pub downlink_high: Option<u64>,
    /// Uplink, Hz, for transceivers and transponders.
    pub uplink_low: Option<u64>,
    /// The top of a transponder's uplink band, Hz.
    pub uplink_high: Option<u64>,
    /// The modulation: FM, BPSK, CW, LoRa, APT...
    pub mode: Option<String>,
    /// Symbols a second, for data modes.
    pub baud: Option<f64>,
    /// Whether a transponder inverts (its downlink band runs the other way).
    pub invert: bool,
    /// Whether it's active and the satellite is alive.
    pub active: bool,
    /// The radio service: Amateur, Meteorological, Space Operation... (often "Unknown").
    pub service: String,
    /// SatNOGS flags it as transmitting outside its coordinated frequency.
    pub frequency_violation: bool,
}

impl Transmitter {
    /// The frequency to tune to, MHz text: `145.800`, `137.6257`, `145.860-145.900`.
    pub fn downlink(&self) -> Option<String> {
        Some(band(self.downlink_low?, self.downlink_high))
    }

    /// Where to transmit to it, MHz text, for transceivers and transponders.
    pub fn uplink(&self) -> Option<String> {
        Some(band(self.uplink_low?, self.uplink_high))
    }
}

/// `145800000` as `145.800`, `137625700` as `137.6257`: MHz, at least three decimals.
pub fn mhz(hz: u64) -> String {
    let s = format!("{:.6}", hz as f64 / 1e6);
    let (whole, frac) = s.split_once('.').unwrap_or((&s, "000000"));
    let frac = frac.trim_end_matches('0');
    format!("{whole}.{frac:0<3}")
}

fn band(low: u64, high: Option<u64>) -> String {
    match high {
        Some(h) if h != low => format!("{}-{}", mhz(low), mhz(h)),
        _ => mhz(low),
    }
}

/// Transmitters from the DB's JSON, every satellite's.
pub fn parse(text: &str) -> Result<Vec<Transmitter>> {
    let v: Value = serde_json::from_str(text).context("SatNOGS DB transmitters")?;
    let Value::Array(items) = v else { bail!("SatNOGS DB transmitters: not a list") };
    let hz = |t: &Value, k: &str| t.get(k).and_then(Value::as_u64).filter(|&f| f > 0);
    let text = |t: &Value, k: &str| t.get(k).and_then(Value::as_str).unwrap_or_default().trim().to_string();
    Ok(items
        .iter()
        .filter_map(|t| {
            Some(Transmitter {
                norad: t.get("norad_cat_id")?.as_u64()?,
                description: text(t, "description"),
                kind: text(t, "type"),
                downlink_low: hz(t, "downlink_low"),
                downlink_high: hz(t, "downlink_high"),
                uplink_low: hz(t, "uplink_low"),
                uplink_high: hz(t, "uplink_high"),
                mode: Some(text(t, "mode")).filter(|m| !m.is_empty() && m != "UNKNOWN"),
                baud: t.get("baud").and_then(Value::as_f64).filter(|&b| b > 0.0),
                invert: t.get("invert").and_then(Value::as_bool).unwrap_or(false),
                active: t.get("alive").and_then(Value::as_bool).unwrap_or(false) && text(t, "status") == "active",
                service: text(t, "service"),
                frequency_violation: t.get("frequency_violation").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect())
}

/// Transmitters by satellite (NORAD number): active ones first, then by downlink.
pub fn by_satellite(all: Vec<Transmitter>) -> HashMap<u64, Vec<Transmitter>> {
    let mut by: HashMap<u64, Vec<Transmitter>> = HashMap::new();
    for t in all {
        by.entry(t.norad).or_default().push(t);
    }
    for v in by.values_mut() {
        v.sort_by_key(|t| (!t.active, t.downlink_low.is_none(), t.downlink_low));
    }
    by
}

/// Every satellite's transmitters: the cached copy while it's under a day old, else the
/// DB's list (see the module docs). Also the copy's age in seconds and, when an old copy
/// stood in, why.
pub async fn load() -> Result<(HashMap<u64, Vec<Transmitter>>, f64, Option<String>)> {
    let path = crate::source::cache_root().join("satnogs").join("transmitters.json");
    let (text, age, note) =
        crate::satellites::fetch_cached("SatNOGS DB", path, URL, Duration::from_secs(24 * 3600), |t| parse(t).is_ok_and(|v| !v.is_empty())).await?;
    Ok((by_satellite(parse(&text)?), age, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three of the ISS's transmitters and one of NOAA 19's, as the DB has them (SatNOGS DB,
    /// CC BY-SA 4.0), trimmed to the fields mapterm reads plus a few it doesn't.
    const SAMPLE: &str = r#"[
      {"uuid":"a","description":"Mode V APRS","alive":true,"type":"Transceiver","uplink_low":145825000,"uplink_high":null,"downlink_low":145825000,"downlink_high":null,"mode":"AFSK","baud":1200.0,"invert":false,"norad_cat_id":25544,"status":"active","service":"Unknown","frequency_violation":false},
      {"uuid":"b","description":"TEMP Off-Frequency SSTV","alive":false,"type":"Transmitter","uplink_low":null,"uplink_high":null,"downlink_low":145480000,"downlink_high":null,"mode":"SSTV","baud":null,"invert":false,"norad_cat_id":25544,"status":"inactive","service":"Unknown","frequency_violation":false},
      {"uuid":"c","description":"Mystery transmitter 137.6257MHz","alive":true,"type":"Transmitter","uplink_low":null,"uplink_high":null,"downlink_low":137625700,"downlink_high":null,"mode":"CW","baud":null,"invert":false,"norad_cat_id":25544,"status":"active","service":"Unknown","frequency_violation":true},
      {"uuid":"d","description":"HRPT Downlink","alive":true,"type":"Transmitter","uplink_low":null,"uplink_high":null,"downlink_low":1698000000,"downlink_high":null,"mode":"HRPT","baud":665400.0,"invert":false,"norad_cat_id":33591,"status":"active","service":"Meteorological","frequency_violation":false},
      {"uuid":"e","description":"Linear transponder","alive":true,"type":"Transponder","uplink_low":435280000,"uplink_high":435300000,"downlink_low":145860000,"downlink_high":145880000,"mode":"USB","baud":null,"invert":true,"norad_cat_id":43017,"status":"active","service":"Amateur","frequency_violation":false},
      {"uuid":"f","description":"no satellite","alive":true,"type":"Transmitter","downlink_low":1,"norad_cat_id":null,"status":"active"}
    ]"#;

    #[test]
    fn reads_the_db() {
        let all = parse(SAMPLE).unwrap();
        assert_eq!(all.len(), 5, "the one with no NORAD number is left out");
        let by = by_satellite(all);
        let iss = &by[&25544];
        assert_eq!(iss.len(), 3);
        assert!(iss[0].active && iss[1].active && !iss[2].active, "active first");
        assert_eq!(iss[0].downlink().as_deref(), Some("137.6257"), "then by frequency");
        assert!(iss[0].frequency_violation);
        assert_eq!(iss[1].mode.as_deref(), Some("AFSK"));
        assert_eq!(iss[1].uplink().as_deref(), Some("145.825"));
        assert_eq!(by[&33591][0].downlink().as_deref(), Some("1698.000"));
        assert_eq!(by[&33591][0].service, "Meteorological");
        let t = &by[&43017][0];
        assert_eq!((t.downlink().as_deref(), t.uplink().as_deref(), t.invert), (Some("145.860-145.880"), Some("435.280-435.300"), true));
    }

    #[test]
    fn megahertz() {
        assert_eq!(mhz(145_800_000), "145.800");
        assert_eq!(mhz(137_625_700), "137.6257");
        assert_eq!(mhz(2_265_000_000), "2265.000");
        assert_eq!(mhz(437_023_000), "437.023");
    }

    #[test]
    fn not_a_list() {
        assert!(parse("{\"detail\":\"Not found.\"}").is_err());
        assert!(parse("<html>").is_err());
    }
}
