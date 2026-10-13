//! The "reading colours" view setting (Acrobat's Replace Document Colors): solid text and
//! line-art paints are remapped for readability while rendering. Display only — the document
//! itself never changes. The setting flows into [`hayro::RenderSettings`] through
//! `RenderConfig`.
//!
//! Night reading ([`NightTheme`]) is the other kind of reading colours: instead of remapping
//! paints to two fixed colours, the finished raster is darkened pixel by pixel. Each pixel's
//! luminosity is inverted while its hue and saturation are kept (the PDF `Color` blend of the
//! inverted page with the original), so white paper turns dark, black text turns light, and a red
//! heading or a blue diagram stays red or blue instead of flipping to its complement. A theme then
//! lifts the black to a softer background and dims the white so text doesn't glare.

/// The reading-colours setting for one document. `params` and `night` both `None` means off;
/// at most one of them is set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadingColors {
    /// The colours and flags, or `None` when the setting is off.
    pub params: Option<ReadingParams>,
    /// Night reading, applied to the finished raster. Settings written before it existed have
    /// no such key and read as off.
    #[serde(default)]
    pub night: Option<NightTheme>,
}

/// The look of night reading: the colour the page's white becomes, and the colour its black
/// becomes (the text).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NightTheme {
    /// Dark grey paper, light grey text: the default, easiest on the eyes.
    DarkGrey,
    /// Black paper (saves power on OLED screens), off-white text.
    Black,
    /// Warm dark brown paper, cream text.
    Sepia,
}

impl NightTheme {
    /// Every theme, in menu order.
    pub const ALL: [NightTheme; 3] = [NightTheme::DarkGrey, NightTheme::Black, NightTheme::Sepia];

    /// The mode name `ReadingColors::parse` takes for this theme.
    pub fn mode_name(self) -> &'static str {
        match self {
            NightTheme::DarkGrey => "night",
            NightTheme::Black => "night-black",
            NightTheme::Sepia => "night-sepia",
        }
    }

    /// The sRGB colour the page's white turns into.
    pub fn background(self) -> [u8; 3] {
        match self {
            NightTheme::DarkGrey => [30, 30, 32],
            NightTheme::Black => [0, 0, 0],
            NightTheme::Sepia => [42, 33, 25],
        }
    }

    /// The sRGB colour the page's black turns into.
    pub fn text(self) -> [u8; 3] {
        match self {
            NightTheme::DarkGrey => [228, 228, 228],
            NightTheme::Black => [235, 235, 235],
            NightTheme::Sepia => [240, 222, 192],
        }
    }
}

/// Darkens a premultiplied RGBA8 raster for night reading, in place. Total for any input: a
/// trailing partial pixel is left alone, and transparent pixels stay transparent.
pub fn apply_night(rgba: &mut [u8], theme: NightTheme) {
    let unit = |c: [u8; 3]| c.map(|v| f32::from(v) / 255.0);
    let (bg, fg) = (unit(theme.background()), unit(theme.text()));
    for px in rgba.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            continue;
        }
        let alpha = f32::from(a) / 255.0;
        // Un-premultiply; `a > 0` here, so the division is finite.
        let mut c = [0.0f32; 3];
        for (k, v) in c.iter_mut().enumerate() {
            *v = (f32::from(px[k]) / 255.0 / alpha).clamp(0.0, 1.0);
        }
        let mut c = invert_luminosity(c);
        for k in 0..3 {
            // Black (the inverted paper) becomes the background and white (the inverted ink)
            // the text colour: [0, 1] maps linearly onto [bg, fg] per channel.
            c[k] = bg[k] + c[k] * (fg[k] - bg[k]);
        }
        for k in 0..3 {
            px[k] = (c[k] * alpha * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
}

/// Luminosity as the PDF blend modes define it (ISO 32000-2 §11.3.5.3).
fn lum(c: [f32; 3]) -> f32 {
    0.30 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// The colour with luminosity `1 − lum(c)` and the hue and saturation of `c`: PDF's
/// `SetLum(c, 1 − Lum(c))`, including its `ClipColor` step that keeps channels in range.
fn invert_luminosity(c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let d = (1.0 - l) - l;
    let c = c.map(|v| v + d);
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    let mut out = c;
    if n < 0.0 && l - n > f32::EPSILON {
        out = out.map(|v| l + (v - l) * l / (l - n));
    }
    if x > 1.0 && x - l > f32::EPSILON {
        out = out.map(|v| l + (v - l) * (1.0 - l) / (x - l));
    }
    out.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 })
}

/// Colours and flags of the reading-colours mapping.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReadingParams {
    /// The sRGB colour that dark paints are mapped towards.
    pub text: [u8; 3],
    /// The sRGB colour that light paints are mapped towards.
    pub background: [u8; 3],
    /// When `true`, only near-black paints are mapped; lighter ones keep their colour.
    pub only_black: bool,
    /// When `true`, line-art paints are mapped in addition to text paints.
    pub line_art: bool,
}

/// Every mode name `ReadingColors::parse` takes, for messages and tool schemas.
pub const MODES: &str = "off, black-on-white, white-on-black, yellow-on-black, green-on-black, night, night-black, night-sepia, custom";

impl ReadingColors {
    /// Black text on a white page.
    pub const BLACK_ON_WHITE: Self = Self {
        params: Some(ReadingParams { text: [0x00, 0x00, 0x00], background: [0xff, 0xff, 0xff], only_black: false, line_art: true }),
        night: None,
    };
    /// White text on a black page.
    pub const WHITE_ON_BLACK: Self = Self {
        params: Some(ReadingParams { text: [0xff, 0xff, 0xff], background: [0x00, 0x00, 0x00], only_black: false, line_art: true }),
        night: None,
    };
    /// Yellow text on a black page.
    pub const YELLOW_ON_BLACK: Self = Self {
        params: Some(ReadingParams { text: [0xff, 0xff, 0x00], background: [0x00, 0x00, 0x00], only_black: false, line_art: true }),
        night: None,
    };
    /// Green text on a black page.
    pub const GREEN_ON_BLACK: Self = Self {
        params: Some(ReadingParams { text: [0x00, 0xff, 0x00], background: [0x00, 0x00, 0x00], only_black: false, line_art: true }),
        night: None,
    };

    /// Parses tool/settings input. `mode` is one of `off | black-on-white | white-on-black |
    /// yellow-on-black | green-on-black | custom`; `custom` needs `text` and `background` as
    /// `#rrggbb`; the flags override the preset defaults when given. `off` ignores colours.
    /// Errors are actionable strings.
    pub fn parse(mode: &str, text: Option<&str>, background: Option<&str>, only_black: Option<bool>, line_art: Option<bool>) -> Result<Self, String> {
        // Every mode accepts the flags: they override the preset defaults, and are the custom
        // mode's own values.
        let with_flags = |mut colors: Self| {
            if let Some(p) = colors.params.as_mut() {
                if let Some(v) = only_black {
                    p.only_black = v;
                }
                if let Some(v) = line_art {
                    p.line_art = v;
                }
            }
            colors
        };
        let night = |theme: NightTheme| Self { params: None, night: Some(theme) };
        match mode {
            "off" => Ok(Self::default()),
            "night" => Ok(night(NightTheme::DarkGrey)),
            "night-black" => Ok(night(NightTheme::Black)),
            "night-sepia" => Ok(night(NightTheme::Sepia)),
            "black-on-white" => Ok(with_flags(Self::BLACK_ON_WHITE)),
            "white-on-black" => Ok(with_flags(Self::WHITE_ON_BLACK)),
            "yellow-on-black" => Ok(with_flags(Self::YELLOW_ON_BLACK)),
            "green-on-black" => Ok(with_flags(Self::GREEN_ON_BLACK)),
            "custom" => match (text, background) {
                (None, None) => Err("custom needs text and background as #rrggbb".into()),
                (None, Some(_)) => Err("custom needs text as #rrggbb".into()),
                (Some(_), None) => Err("custom needs background as #rrggbb".into()),
                (Some(text), Some(background)) => Ok(with_flags(Self {
                    params: Some(ReadingParams {
                        text: parse_hex("text", text)?,
                        background: parse_hex("background", background)?,
                        only_black: false,
                        line_art: true,
                    }),
                    night: None,
                })),
            },
            other => Err(format!("unknown mode {other:?}; expected one of: {MODES}")),
        }
    }

    /// The mode name for echoes and display: `"off"`, a preset name, a night theme's name, or
    /// `"custom"`.
    pub fn mode_name(&self) -> &'static str {
        if let Some(theme) = self.night {
            return theme.mode_name();
        }
        match self.params {
            None => "off",
            Some(params) => {
                if Some(params) == Self::BLACK_ON_WHITE.params {
                    "black-on-white"
                } else if Some(params) == Self::WHITE_ON_BLACK.params {
                    "white-on-black"
                } else if Some(params) == Self::YELLOW_ON_BLACK.params {
                    "yellow-on-black"
                } else if Some(params) == Self::GREEN_ON_BLACK.params {
                    "green-on-black"
                } else {
                    "custom"
                }
            }
        }
    }

    /// The text colour as lowercase `#rrggbb`, or `None` when off.
    pub fn text_hex(&self) -> Option<String> {
        self.params.map(|p| p.text).or(self.night.map(NightTheme::text)).map(hex)
    }

    /// The background colour as lowercase `#rrggbb`, or `None` when off.
    pub fn background_hex(&self) -> Option<String> {
        self.params.map(|p| p.background).or(self.night.map(NightTheme::background)).map(hex)
    }

    /// `true` when either kind of reading colours is on.
    pub fn is_on(&self) -> bool {
        self.params.is_some() || self.night.is_some()
    }
}

impl From<ReadingParams> for hayro::ColorReplacement {
    fn from(p: ReadingParams) -> Self {
        hayro::ColorReplacement { text: p.text, background: p.background, only_black: p.only_black, line_art: p.line_art }
    }
}

/// Parses a colour given as `#rrggbb` — exactly this shape, either case of digits. The error
/// names the argument and the expected shape.
fn parse_hex(name: &str, value: &str) -> Result<[u8; 3], String> {
    let invalid = || format!("invalid {name} colour {value:?}; expected #rrggbb (six hex digits, e.g. #00ff00)");
    let Some(digits) = value.strip_prefix('#') else { return Err(invalid()) };
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid());
    }
    let mut rgb = [0u8; 3];
    for (i, channel) in rgb.iter_mut().enumerate() {
        // Length and charset are checked above, so the parse cannot actually fail; the mapping
        // only keeps it total without panicking.
        *channel = u8::from_str_radix(&digits[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
    }
    Ok(rgb)
}

/// Formats sRGB bytes as lowercase `#rrggbb`.
fn hex(rgb: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_mode() {
        let off = ReadingColors::parse("off", None, None, None, None).unwrap();
        assert_eq!(off, ReadingColors::default());
        assert_eq!(off.mode_name(), "off");
        assert_eq!(off.text_hex(), None);
        assert_eq!(off.background_hex(), None);
        // `off` ignores colour and flag arguments.
        assert_eq!(ReadingColors::parse("off", Some("zzz"), Some("#000000"), Some(true), Some(false)).unwrap(), off);

        for (mode, text, background) in [
            ("black-on-white", "#000000", "#ffffff"),
            ("white-on-black", "#ffffff", "#000000"),
            ("yellow-on-black", "#ffff00", "#000000"),
            ("green-on-black", "#00ff00", "#000000"),
        ] {
            let colors = ReadingColors::parse(mode, None, None, None, None).unwrap();
            assert_eq!(colors.mode_name(), mode, "{mode}");
            assert_eq!(colors.text_hex().as_deref(), Some(text), "{mode}");
            assert_eq!(colors.background_hex().as_deref(), Some(background), "{mode}");
            let params = colors.params.unwrap();
            assert!(!params.only_black && params.line_art, "{mode}");
        }

        // The flags override the preset defaults; a changed preset is no longer one.
        let overridden = ReadingColors::parse("white-on-black", None, None, Some(true), Some(false)).unwrap();
        let params = overridden.params.unwrap();
        assert!(params.only_black && !params.line_art);
        assert_eq!(overridden.mode_name(), "custom");

        // `custom` takes either case of hex and echoes lowercase.
        let custom = ReadingColors::parse("custom", Some("#AaBbCc"), Some("#123456"), Some(true), None).unwrap();
        assert_eq!(custom.mode_name(), "custom");
        assert_eq!(custom.text_hex().as_deref(), Some("#aabbcc"));
        assert_eq!(custom.background_hex().as_deref(), Some("#123456"));
        let params = custom.params.unwrap();
        assert!(params.only_black && params.line_art);
    }

    #[test]
    fn parse_rejects_unknown_mode() {
        assert_eq!(
            ReadingColors::parse("nope", None, None, None, None),
            Err("unknown mode \"nope\"; expected one of: off, black-on-white, white-on-black, yellow-on-black, green-on-black, night, night-black, night-sepia, custom".to_string())
        );
    }

    #[test]
    fn parse_rejects_malformed_hex() {
        for bad in ["zzz", "000000", "#000", "#0000000", "#00g000", ""] {
            assert_eq!(
                ReadingColors::parse("custom", Some(bad), Some("#000000"), None, None),
                Err(format!("invalid text colour {bad:?}; expected #rrggbb (six hex digits, e.g. #00ff00)")),
                "{bad}"
            );
        }
        assert_eq!(
            ReadingColors::parse("custom", Some("#000000"), Some("ffffff"), None, None),
            Err("invalid background colour \"ffffff\"; expected #rrggbb (six hex digits, e.g. #00ff00)".to_string())
        );
    }

    #[test]
    fn parse_rejects_custom_missing_colours() {
        assert_eq!(ReadingColors::parse("custom", None, None, None, None), Err("custom needs text and background as #rrggbb".to_string()));
        assert_eq!(ReadingColors::parse("custom", None, Some("#000000"), None, None), Err("custom needs text as #rrggbb".to_string()));
        assert_eq!(ReadingColors::parse("custom", Some("#000000"), None, None, None), Err("custom needs background as #rrggbb".to_string()));
    }

    fn px(rgb: [u8; 3]) -> [u8; 4] {
        [rgb[0], rgb[1], rgb[2], 255]
    }

    fn night(theme: NightTheme, rgba: [u8; 4]) -> [u8; 4] {
        let mut buf = rgba;
        apply_night(&mut buf, theme);
        buf
    }

    #[test]
    fn night_turns_paper_and_ink_into_the_theme_colours() {
        for theme in NightTheme::ALL {
            assert_eq!(night(theme, px([255, 255, 255])), px(theme.background()), "{theme:?} white");
            let ink = night(theme, px([0, 0, 0]));
            for (k, v) in ink.iter().take(3).enumerate() {
                assert!(v.abs_diff(theme.text()[k]) <= 1, "{theme:?} black → {ink:?}");
            }
        }
    }

    #[test]
    fn night_keeps_hues() {
        // A red heading stays red (red is the strongest channel), and a blue box stays blue,
        // where a plain inversion would make them cyan and orange.
        let red = night(NightTheme::DarkGrey, px([210, 30, 30]));
        assert!(red[0] > red[1] && red[0] > red[2], "{red:?}");
        let blue = night(NightTheme::DarkGrey, px([50, 100, 230]));
        assert!(blue[2] > blue[0] && blue[2] > blue[1], "{blue:?}");
        // Light colours turn dark and dark ones light.
        let yellow = night(NightTheme::Black, px([255, 245, 100]));
        assert!(yellow.iter().take(3).map(|&v| u32::from(v)).sum::<u32>() < 300, "{yellow:?}");
        let navy = night(NightTheme::Black, px([10, 20, 90]));
        assert!(navy.iter().take(3).map(|&v| u32::from(v)).sum::<u32>() > 400, "{navy:?}");
    }

    #[test]
    fn night_is_total_on_odd_buffers() {
        // Transparent pixels stay transparent, premultiplied ones stay premultiplied, a trailing
        // partial pixel is left alone, and nothing panics.
        let mut buf = vec![0u8, 0, 0, 0, 64, 64, 64, 128, 9, 9];
        apply_night(&mut buf, NightTheme::Sepia);
        assert_eq!(&buf[..4], &[0, 0, 0, 0]);
        assert_eq!(buf[7], 128);
        assert!(buf[4..7].iter().all(|&v| v <= 128));
        assert_eq!(&buf[8..], &[9, 9]);
        let mut empty: [u8; 0] = [];
        apply_night(&mut empty, NightTheme::Black);
        // Premultiplied channels above alpha (malformed) are clamped, not overflowed.
        let mut bad = [255u8, 255, 255, 10];
        apply_night(&mut bad, NightTheme::DarkGrey);
        assert!(bad.iter().take(3).all(|&v| v <= 10));
    }

    #[test]
    fn parse_knows_night_themes_and_they_echo() {
        for theme in NightTheme::ALL {
            let colors = ReadingColors::parse(theme.mode_name(), None, None, None, None).unwrap();
            assert_eq!(colors, ReadingColors { params: None, night: Some(theme) });
            assert_eq!(colors.mode_name(), theme.mode_name());
            assert!(colors.is_on());
            assert!(colors.background_hex().is_some() && colors.text_hex().is_some());
        }
        assert!(!ReadingColors::default().is_on());
    }

    #[test]
    fn settings_saved_before_night_reading_still_load() {
        let old: ReadingColors = serde_json::from_str(r#"{"params":null}"#).unwrap();
        assert_eq!(old, ReadingColors::default());
        let saved = serde_json::to_string(&ReadingColors { params: None, night: Some(NightTheme::Sepia) }).unwrap();
        assert_eq!(serde_json::from_str::<ReadingColors>(&saved).unwrap().night, Some(NightTheme::Sepia));
    }
}
