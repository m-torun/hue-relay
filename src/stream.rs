//! HueStream (Entertainment API v2) frames and per-channel gains.

use std::collections::HashMap;

/// Area id -> gain per channel id.
pub type Gains = HashMap<String, Vec<f64>>;

const HEADER_V2: usize = 52; // "HueStream", version, sequence, reserved, colour space, reserved, area id (36)
const CHANNEL: usize = 7; // channel id, three 16-bit values

pub const XYB: u8 = 1;

/// What `apply_gains` found in a frame.
#[derive(Debug, PartialEq)]
pub enum Frame {
    /// Not a HueStream frame.
    Invalid,
    /// A frame of another protocol version, passed on unchanged.
    Other(Vec<u8>),
    /// A v2 frame for `area`, with gains applied.
    V2 { area: String, xyb: bool, out: Vec<u8> },
}

/// Scale each channel's brightness by the area's gain: in XYB frames the brightness, in RGB frames
/// all three values.
pub fn apply_gains(pkt: &[u8], gains: &Gains) -> Frame {
    if pkt.len() < 16 || !pkt.starts_with(b"HueStream") {
        return Frame::Invalid;
    }
    if pkt[9] != 2 {
        return Frame::Other(pkt.to_vec());
    }
    if pkt.len() < HEADER_V2 {
        return Frame::Invalid;
    }
    let area = String::from_utf8_lossy(&pkt[16..HEADER_V2]).into_owned();
    let xyb = pkt[14] == XYB;
    let mut out = pkt.to_vec();
    if let Some(g) = gains.get(&area) {
        let mut off = HEADER_V2;
        while off + CHANNEL <= out.len() {
            let gain = g.get(out[off] as usize).copied().unwrap_or(1.0);
            if gain != 1.0 {
                let first = if xyb { 2 } else { 0 };
                for i in first..3 {
                    let at = off + 1 + 2 * i;
                    let v = u16::from_be_bytes([out[at], out[at + 1]]) as f64;
                    let scaled = (v * gain).clamp(0.0, 65535.0) as u16;
                    out[at..at + 2].copy_from_slice(&scaled.to_be_bytes());
                }
            }
            off += CHANNEL;
        }
    }
    Frame::V2 { area, xyb, out }
}

/// A v2 XYB frame with a colour and brightness (0-1) per channel: (channel id, x, y, brightness).
pub fn xyb_frame_per_channel(area: &str, channels: &[(u8, f64, f64, f64)]) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(HEADER_V2 + CHANNEL * channels.len());
    pkt.extend_from_slice(b"HueStream");
    pkt.extend_from_slice(&[2, 0, 0, 0, 0, XYB, 0]);
    pkt.extend_from_slice(area.as_bytes());
    let q = |v: f64| ((v.clamp(0.0, 1.0)) * 65535.0) as u16;
    for &(ch, x, y, b) in channels {
        pkt.push(ch);
        for v in [q(x), q(y), q(b)] {
            pkt.extend_from_slice(&v.to_be_bytes());
        }
    }
    pkt
}

/// CIE xy of a black body at 1e6/mirek kelvin (Kim et al. approximation of the Planckian locus).
pub fn mirek_to_xy(mirek: f64) -> (f64, f64) {
    let t = 1e6 / mirek.clamp(100.0, 600.0);
    let x = if t <= 4000.0 {
        -0.2661239e9 / t.powi(3) - 0.2343589e6 / t.powi(2) + 0.8776956e3 / t + 0.179910
    } else {
        -3.0258469e9 / t.powi(3) + 2.1070379e6 / t.powi(2) + 0.2226347e3 / t + 0.240390
    };
    let y = if t <= 2222.0 {
        -1.1063814 * x.powi(3) - 1.34811020 * x.powi(2) + 2.18555832 * x - 0.20219683
    } else if t <= 4000.0 {
        -0.9549476 * x.powi(3) - 1.37418593 * x.powi(2) + 2.09137015 * x - 0.16748867
    } else {
        3.0817580 * x.powi(3) - 5.87338670 * x.powi(2) + 3.75112997 * x - 0.37001483
    };
    (x, y)
}

/// The channels of a v2 frame as (channel id, three values), for logs.
pub fn channels(pkt: &[u8]) -> Vec<(u8, [u16; 3])> {
    let mut v = Vec::new();
    let mut off = HEADER_V2;
    while off + CHANNEL <= pkt.len() {
        let val = |i: usize| u16::from_be_bytes([pkt[off + 1 + 2 * i], pkt[off + 2 + 2 * i]]);
        v.push((pkt[off], [val(0), val(1), val(2)]));
        off += CHANNEL;
    }
    v
}

/// A v2 XYB frame giving every channel the same colour and brightness (0-1).
pub fn xyb_frame(area: &str, channels: &[u8], x: f64, y: f64, b: f64) -> Vec<u8> {
    let mut pkt = Vec::with_capacity(HEADER_V2 + CHANNEL * channels.len());
    pkt.extend_from_slice(b"HueStream");
    pkt.extend_from_slice(&[2, 0, 0, 0, 0, XYB, 0]);
    pkt.extend_from_slice(area.as_bytes());
    let q = |v: f64| ((v.clamp(0.0, 1.0)) * 65535.0) as u16;
    for &ch in channels {
        pkt.push(ch);
        for v in [q(x), q(y), q(b)] {
            pkt.extend_from_slice(&v.to_be_bytes());
        }
    }
    pkt
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: &str = "00000000-1111-2222-3333-444444444444";

    #[test]
    fn xyb_gains_scale_brightness_only() {
        let pkt = xyb_frame(AREA, &[0, 1, 2], 0.5, 0.4, 1.0);
        let gains: Gains = [(AREA.to_string(), vec![0.5, 1.0])].into();
        let Frame::V2 { area, xyb, out } = apply_gains(&pkt, &gains) else {
            panic!()
        };
        assert_eq!(area, AREA);
        assert!(xyb);
        let ch = channels(&out);
        assert_eq!(ch[0].1[0], ch[1].1[0]); // x untouched
        assert_eq!(ch[0].1[2], 32767); // brightness halved
        assert_eq!(ch[1].1[2], 65535); // gain 1
        assert_eq!(ch[2].1[2], 65535); // no gain listed: 1
    }

    #[test]
    fn rgb_gains_scale_all_three_and_clamp() {
        let mut pkt = xyb_frame(AREA, &[0], 0.0, 0.0, 0.0);
        pkt[14] = 0; // RGB
        pkt[53..59].copy_from_slice(&[0xff, 0xff, 0x80, 0x00, 0x00, 0x10]);
        let gains: Gains = [(AREA.to_string(), vec![2.0])].into();
        let Frame::V2 { out, xyb, .. } = apply_gains(&pkt, &gains) else {
            panic!()
        };
        assert!(!xyb);
        assert_eq!(channels(&out)[0].1, [65535, 65535, 32]);
    }

    #[test]
    fn warm_white_on_the_locus() {
        let (x, y) = mirek_to_xy(366.0);
        assert!((x - 0.4567).abs() < 0.001 && (y - 0.4101).abs() < 0.001, "{x} {y}");
    }

    #[test]
    fn other_frames() {
        assert_eq!(apply_gains(b"nonsense", &Gains::new()), Frame::Invalid);
        let mut v1 = b"HueStream".to_vec();
        v1.extend_from_slice(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        assert!(matches!(apply_gains(&v1, &Gains::new()), Frame::Other(_)));
    }
}
