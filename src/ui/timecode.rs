use crate::model::project::FrameRate;

const NS_PER_SECOND: u128 = 1_000_000_000;

pub fn nominal_fps(frame_rate: &FrameRate) -> u64 {
    frame_rate.as_f64().round().max(1.0) as u64
}

pub fn format_ns_as_timecode(ns: u64, frame_rate: &FrameRate) -> String {
    let fps = nominal_fps(frame_rate).max(1);
    let fps_num = u128::from(frame_rate.numerator.max(1));
    let fps_den = u128::from(frame_rate.denominator.max(1));
    let total_frames = (u128::from(ns) * fps_num) / (NS_PER_SECOND * fps_den);
    let total_frames = total_frames.min(u128::from(u64::MAX)) as u64;
    let ff = total_frames % fps;
    let total_secs = total_frames / fps;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    format!("{h:02}:{m:02}:{s:02}:{ff:02}")
}

pub fn parse_timecode_to_ns(input: &str, frame_rate: &FrameRate) -> Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("Enter a timecode like HH:MM:SS:FF.".to_string());
    }

    let normalized = trimmed.replace(';', ":");
    let parts: Vec<&str> = normalized.split(':').collect();
    let (h, m, s, f) = match parts.len() {
        4 => (
            parse_component(parts[0], "hours")?,
            parse_component(parts[1], "minutes")?,
            parse_component(parts[2], "seconds")?,
            parse_component(parts[3], "frames")?,
        ),
        3 => (
            0,
            parse_component(parts[0], "minutes")?,
            parse_component(parts[1], "seconds")?,
            parse_component(parts[2], "frames")?,
        ),
        _ => {
            return Err("Expected HH:MM:SS:FF (or MM:SS:FF).".to_string());
        }
    };

    if m >= 60 || s >= 60 {
        return Err("Minutes and seconds must be between 0 and 59.".to_string());
    }

    let fps = nominal_fps(frame_rate).max(1);
    if f >= fps {
        return Err(format!(
            "Frame value must be between 0 and {} for this frame rate.",
            fps.saturating_sub(1)
        ));
    }

    let total_secs = h
        .checked_mul(3600)
        .and_then(|v| v.checked_add(m.checked_mul(60)?))
        .and_then(|v| v.checked_add(s))
        .ok_or_else(|| "Timecode value is too large.".to_string())?;

    let total_frames = total_secs
        .checked_mul(fps)
        .and_then(|v| v.checked_add(f))
        .ok_or_else(|| "Timecode value is too large.".to_string())?;

    let fps_num = u128::from(frame_rate.numerator.max(1));
    let fps_den = u128::from(frame_rate.denominator.max(1));
    let ns = u128::from(total_frames)
        .checked_mul(NS_PER_SECOND)
        .and_then(|v| v.checked_mul(fps_den))
        .and_then(|v| v.checked_div(fps_num))
        .ok_or_else(|| "Timecode conversion failed.".to_string())?;

    u64::try_from(ns).map_err(|_| "Timecode value is too large.".to_string())
}

fn parse_component(part: &str, label: &str) -> Result<u64, String> {
    part.parse::<u64>()
        .map_err(|_| format!("Invalid {label} component: '{part}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fps_24() -> FrameRate {
        FrameRate {
            numerator: 24,
            denominator: 1,
        }
    }

    #[test]
    fn format_timecode_24fps() {
        let fr = fps_24();
        assert_eq!(format_ns_as_timecode(0, &fr), "00:00:00:00");
        assert_eq!(format_ns_as_timecode(1_500_000_000, &fr), "00:00:01:12");
    }

    #[test]
    fn parse_full_timecode_24fps() {
        let fr = fps_24();
        let ns = parse_timecode_to_ns("00:00:10:12", &fr).expect("parse should succeed");
        assert_eq!(format_ns_as_timecode(ns, &fr), "00:00:10:12");
    }

    #[test]
    fn parse_mm_ss_ff_timecode() {
        let fr = fps_24();
        let ns = parse_timecode_to_ns("01:02:03", &fr).expect("parse should succeed");
        assert_eq!(format_ns_as_timecode(ns, &fr), "00:01:02:03");
    }

    #[test]
    fn parse_rejects_out_of_range_frames() {
        let fr = fps_24();
        let err =
            parse_timecode_to_ns("00:00:01:24", &fr).expect_err("should reject ff=24 at 24fps");
        assert!(err.contains("Frame value"));
    }

    #[test]
    fn parse_semicolon_round_trip_ntsc() {
        let fr = FrameRate {
            numerator: 30000,
            denominator: 1001,
        };
        let ns = parse_timecode_to_ns("00:00:10;15", &fr).expect("parse should succeed");
        assert_eq!(format_ns_as_timecode(ns, &fr), "00:00:10:15");
    }
}
