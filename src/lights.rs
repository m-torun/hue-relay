//! The lights' own state, as the bridge reports it, and how to show it on an area's channels.
//!
//! Between syncs the relay can keep streaming to an area so its per-channel levels still apply: it
//! renders each light's stored state (on, brightness, colour, gradient) onto that light's channels.
//! The bridge accepts and announces commands for a light while it streams, so every app keeps
//! controlling the light as usual; only what the light shows goes through the relay.

use std::collections::HashMap;

use serde_json::Value;

use crate::stream;

/// What a light has been told to show.
#[derive(Clone, Debug, PartialEq)]
pub struct LightState {
    pub on: bool,
    /// 0-100, as the bridge has it.
    pub brightness: f64,
    pub xy: Option<(f64, f64)>,
    pub mirek: Option<f64>,
    /// Gradient points (xy) and mode, when the light shows a gradient.
    pub gradient: Vec<(f64, f64)>,
    pub gradient_mode: String,
}

impl Default for LightState {
    fn default() -> Self {
        LightState {
            on: false,
            brightness: 100.0,
            xy: None,
            mirek: None,
            gradient: Vec::new(),
            gradient_mode: "interpolated_palette".into(),
        }
    }
}

impl LightState {
    /// Merge a full light resource or a partial update from the event stream.
    pub fn merge(&mut self, v: &Value) {
        if let Some(on) = v["on"]["on"].as_bool() {
            self.on = on;
        }
        if let Some(b) = v["dimming"]["brightness"].as_f64() {
            self.brightness = b;
        }
        if let (Some(x), Some(y)) = (v["color"]["xy"]["x"].as_f64(), v["color"]["xy"]["y"].as_f64()) {
            self.xy = Some((x, y));
        }
        if v.get("color_temperature").is_some() {
            let ct = &v["color_temperature"];
            if ct["mirek_valid"].as_bool() == Some(false) || ct["mirek"].is_null() && ct.get("mirek").is_some() {
                self.mirek = None;
            } else if let Some(m) = ct["mirek"].as_f64() {
                self.mirek = Some(m);
            }
        }
        if let Some(g) = v.get("gradient") {
            if let Some(points) = g["points"].as_array() {
                self.gradient = points
                    .iter()
                    .filter_map(|p| Some((p["color"]["xy"]["x"].as_f64()?, p["color"]["xy"]["y"].as_f64()?)))
                    .collect();
            }
            if let Some(m) = g["mode"].as_str() {
                self.gradient_mode = m.to_string();
            }
        }
    }

    /// Colour and brightness (0-1) at position t (0-1) along the light.
    pub fn at(&self, t: f64) -> (f64, f64, f64) {
        let b = if self.on {
            (self.brightness / 100.0).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (x, y) = if self.gradient.len() >= 2 {
            gradient_at(&self.gradient, &self.gradient_mode, t)
        } else if let Some(xy) = self.xy {
            xy
        } else {
            stream::mirek_to_xy(self.mirek.unwrap_or(366.0))
        };
        (x, y, b)
    }
}

/// Colour along a gradient: points evenly spread from one end to the other.
pub fn gradient_at(points: &[(f64, f64)], mode: &str, t: f64) -> (f64, f64) {
    let n = points.len();
    let t = t.clamp(0.0, 1.0);
    match mode {
        "segmented_palette" | "random_pixelated" => points[((t * n as f64) as usize).min(n - 1)],
        _ => {
            let t = if mode == "interpolated_palette_mirrored" {
                1.0 - (2.0 * t - 1.0).abs()
            } else {
                t
            };
            let pos = t * (n - 1) as f64;
            let i = (pos.floor() as usize).min(n - 2);
            let f = pos - i as f64;
            let (a, b) = (points[i], points[i + 1]);
            (a.0 + (b.0 - a.0) * f, a.1 + (b.1 - a.1) * f)
        }
    }
}

/// Where each channel of an area sits: which light, and how far along it (0-1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AreaMap {
    pub channels: Vec<(u8, String, f64)>,
}

impl AreaMap {
    /// From the bridge's entertainment_configuration item, its entertainment resources and lights.
    pub fn build(area: &Value, entertainment: &[Value], lights: &[Value]) -> AreaMap {
        let ent: HashMap<&str, &Value> = entertainment
            .iter()
            .filter_map(|e| Some((e["id"].as_str()?, e)))
            .collect();
        let pixels: HashMap<&str, f64> = lights
            .iter()
            .filter_map(|l| Some((l["id"].as_str()?, l["gradient"]["pixel_count"].as_f64()?)))
            .collect();
        let mut channels = Vec::new();
        for ch in area["channels"].as_array().into_iter().flatten() {
            let Some(id) = ch["channel_id"].as_u64() else { continue };
            let member = &ch["members"][0];
            let Some(e) = member["service"]["rid"].as_str().and_then(|r| ent.get(r)) else {
                continue;
            };
            let Some(light) = e["renderer_reference"]["rid"].as_str() else {
                continue;
            };
            let index = member["index"].as_u64().unwrap_or(0) as usize;
            let segs = e["segments"]["segments"].as_array();
            let t = match segs.and_then(|s| s.get(index)) {
                Some(seg) => {
                    let total = pixels.get(light).copied().unwrap_or_else(|| {
                        segs.unwrap()
                            .iter()
                            .map(|s| s["start"].as_f64().unwrap_or(0.0) + s["length"].as_f64().unwrap_or(1.0))
                            .fold(1.0, f64::max)
                    });
                    (seg["start"].as_f64().unwrap_or(0.0) + seg["length"].as_f64().unwrap_or(1.0) / 2.0) / total
                }
                None => 0.5,
            };
            channels.push((id as u8, light.to_string(), t));
        }
        AreaMap { channels }
    }

    pub fn lights(&self) -> Vec<String> {
        let mut l: Vec<String> = self.channels.iter().map(|c| c.1.clone()).collect();
        l.sort();
        l.dedup();
        l
    }

    /// The colour and brightness of each channel for the lights' current state.
    pub fn render(&self, states: &HashMap<String, LightState>) -> Vec<(u8, f64, f64, f64)> {
        self.channels
            .iter()
            .map(|(ch, light, t)| {
                let (x, y, b) = states.get(light).map(|s| s.at(*t)).unwrap_or((0.3127, 0.3290, 0.0));
                (*ch, x, y, b)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn merges_full_and_partial_updates() {
        let mut s = LightState::default();
        s.merge(&json!({"on": {"on": true}, "dimming": {"brightness": 60.0},
            "color": {"xy": {"x": 0.45, "y": 0.41}}, "color_temperature": {"mirek": 366, "mirek_valid": true}}));
        assert!(s.on && s.brightness == 60.0 && s.xy == Some((0.45, 0.41)) && s.mirek == Some(366.0));
        s.merge(&json!({"dimming": {"brightness": 20.16}}));
        assert_eq!(s.brightness, 20.16);
        s.merge(&json!({"on": {"on": false}}));
        assert_eq!(s.at(0.5).2, 0.0);
    }

    #[test]
    fn gradients() {
        let p = [(0.1, 0.1), (0.5, 0.5)];
        assert_eq!(gradient_at(&p, "interpolated_palette", 0.0), (0.1, 0.1));
        let mid = gradient_at(&p, "interpolated_palette", 0.5);
        assert!((mid.0 - 0.3).abs() < 1e-9);
        assert_eq!(gradient_at(&p, "interpolated_palette_mirrored", 1.0), (0.1, 0.1));
        assert_eq!(gradient_at(&p, "segmented_palette", 0.49), (0.1, 0.1));
        assert_eq!(gradient_at(&p, "segmented_palette", 0.51), (0.5, 0.5));
    }

    #[test]
    fn maps_channels_to_lights_and_positions() {
        // The Flux strip: 14 pixels in 7 segments, one channel per segment.
        let segs = [(0, 1), (1, 2), (3, 3), (6, 2), (8, 3), (11, 2), (13, 1)];
        let ent = vec![json!({"id": "e1", "renderer_reference": {"rid": "L"},
            "segments": {"segments": segs.iter().map(|(s, l)| json!({"start": s, "length": l})).collect::<Vec<_>>()}})];
        let lights = vec![json!({"id": "L", "gradient": {"pixel_count": 14}})];
        let area = json!({"channels": (0..7).map(|i| json!({"channel_id": i,
            "members": [{"service": {"rid": "e1", "rtype": "entertainment"}, "index": i}]})).collect::<Vec<_>>()});
        let m = AreaMap::build(&area, &ent, &lights);
        assert_eq!(m.channels.len(), 7);
        assert!((m.channels[0].2 - 0.5 / 14.0).abs() < 1e-9);
        assert!((m.channels[6].2 - 13.5 / 14.0).abs() < 1e-9);
        assert_eq!(m.lights(), vec!["L".to_string()]);
        let mut states = HashMap::new();
        let mut s = LightState::default();
        s.merge(&json!({"on": {"on": true}, "dimming": {"brightness": 50.0}, "color": {"xy": {"x": 0.4, "y": 0.4}}}));
        states.insert("L".to_string(), s);
        assert!(m.render(&states).iter().all(|c| c.3 == 0.5 && c.1 == 0.4));
    }
}
