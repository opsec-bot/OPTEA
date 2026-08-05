//! Surgical INI editing.
//!
//! Siege's `GameSettings.ini` is not just settings — it carries the user's
//! sensitivity, per-scope ADS values, keybind-adjacent toggles, audio device
//! ids, and a block of explanatory comments the game itself wrote. A
//! parse-into-a-map-and-regenerate round trip would silently discard the
//! comments, reorder keys, normalise number formatting (`50.000000` → `50`), and
//! rewrite all 227 CRLF endings as LF.
//!
//! So this type never regenerates. It holds the original lines verbatim and
//! rewrites **only the value span of the specific line** being changed.
//! Everything else — comments, blank lines, key spelling, section order, line
//! endings — comes back out exactly as it went in.

use std::collections::BTreeMap;

/// A parsed INI document that remembers its exact original text.
#[derive(Debug, Clone)]
pub struct IniDocument {
    /// Lines with their terminators stripped.
    lines: Vec<String>,
    /// The dominant line ending, reproduced on write.
    line_ending: String,
    /// True when the original text ended with a newline.
    trailing_newline: bool,
    /// `(section_lower, key_lower)` → index into `lines`.
    index: BTreeMap<(String, String), usize>,
}

/// Where a value lives on its line.
struct ValueSpan {
    start: usize,
    end: usize,
}

impl IniDocument {
    pub fn parse(text: &str) -> Self {
        // Prefer CRLF when present at all: mixed endings are rare, and matching
        // the dominant style is what keeps a diff to one line.
        let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" }.to_string();
        let trailing_newline = text.ends_with('\n');

        let lines: Vec<String> = text
            .split('\n')
            .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
            .collect();
        // `split` yields a trailing empty element for text ending in a newline;
        // drop it so it is not re-emitted as a blank line on write.
        let lines = if trailing_newline && lines.last().is_some_and(|l| l.is_empty()) {
            lines[..lines.len() - 1].to_vec()
        } else {
            lines
        };

        let mut index = BTreeMap::new();
        let mut section = String::new();
        for (i, line) in lines.iter().enumerate() {
            let t = line.trim();
            if t.starts_with(';') || t.starts_with('#') || t.is_empty() {
                continue;
            }
            if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name.trim().to_lowercase();
                continue;
            }
            if let Some(eq) = line.find('=') {
                let key = line[..eq].trim().to_lowercase();
                if !key.is_empty() {
                    // First occurrence wins, matching how INI readers behave.
                    index.entry((section.clone(), key)).or_insert(i);
                }
            }
        }

        IniDocument {
            lines,
            line_ending,
            trailing_newline,
            index,
        }
    }

    /// Reproduce the document. With no edits this is byte-identical to the input.
    pub fn to_string(&self) -> String {
        let mut s = self.lines.join(&self.line_ending);
        if self.trailing_newline {
            s.push_str(&self.line_ending);
        }
        s
    }

    pub fn line_ending(&self) -> &str {
        &self.line_ending
    }

    fn locate(&self, section: &str, key: &str) -> Option<usize> {
        self.index
            .get(&(section.to_lowercase(), key.to_lowercase()))
            .copied()
    }

    /// The value span on a line, excluding surrounding whitespace.
    fn value_span(line: &str) -> Option<ValueSpan> {
        let eq = line.find('=')?;
        let after = eq + 1;
        let rest = &line[after..];
        let lead = rest.len() - rest.trim_start().len();
        let start = after + lead;
        let end = after + rest.trim_end().len();
        Some(ValueSpan {
            start,
            end: end.max(start),
        })
    }

    /// Current value, if the key exists.
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let idx = self.locate(section, key)?;
        let line = &self.lines[idx];
        let span = Self::value_span(line)?;
        Some(&line[span.start..span.end])
    }

    pub fn get_f64(&self, section: &str, key: &str) -> Option<f64> {
        self.get(section, key)?.parse().ok()
    }

    pub fn get_i64(&self, section: &str, key: &str) -> Option<i64> {
        let raw = self.get(section, key)?;
        raw.parse()
            .ok()
            // Some values are written as floats even when conceptually integral.
            .or_else(|| raw.parse::<f64>().ok().map(|f| f as i64))
    }

    pub fn contains(&self, section: &str, key: &str) -> bool {
        self.locate(section, key).is_some()
    }

    /// Replace a value in place.
    ///
    /// Returns `false` when the key is absent — this never *adds* keys, because
    /// inventing a setting Siege did not write is a good way to have the game
    /// reject or rewrite the file.
    pub fn set(&mut self, section: &str, key: &str, value: &str) -> bool {
        let Some(idx) = self.locate(section, key) else {
            return false;
        };
        let line = &self.lines[idx];
        let Some(span) = Self::value_span(line) else {
            return false;
        };
        // Splice only the value span, so key spelling, spacing around `=`, and
        // anything trailing survive untouched.
        let mut updated = String::with_capacity(line.len() + value.len());
        updated.push_str(&line[..span.start]);
        updated.push_str(value);
        updated.push_str(&line[span.end..]);
        self.lines[idx] = updated;
        true
    }

    /// Every `(section, key, value)` triple, in file order.
    pub fn entries(&self) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        let mut section = String::new();
        for line in &self.lines {
            let t = line.trim();
            if t.starts_with(';') || t.starts_with('#') || t.is_empty() {
                continue;
            }
            if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                section = name.trim().to_string();
                continue;
            }
            if let Some(span) = Self::value_span(line) {
                let key = line[..line.find('=').unwrap()].trim().to_string();
                out.push((section.clone(), key, line[span.start..span.end].to_string()));
            }
        }
        out
    }

    pub fn sections(&self) -> Vec<String> {
        let mut out = Vec::new();
        for line in &self.lines {
            let t = line.trim();
            if let Some(name) = t.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                out.push(name.trim().to_string());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the real file: CRLF, comments, float formatting, empty values.
    const SAMPLE: &str = "[GENERAL]\r\n\
Version=1\r\n\
\r\n\
[DISPLAY_SETTINGS]\r\n\
;WindowMode => 0 fullscreen / 1 windowed / 2 borderless\r\n\
ResolutionWidth=1920\r\n\
ResolutionHeight=1080\r\n\
RefreshRate=143.912003\r\n\
WindowMode=2\r\n\
VSync=0\r\n\
MaxGPUBufferedFrame=1\r\n\
DefaultFOV=90.000000\r\n\
VulkanWhitelistedLayers=\r\n\
";

    #[test]
    fn round_trip_without_edits_is_byte_identical() {
        let doc = IniDocument::parse(SAMPLE);
        assert_eq!(doc.to_string(), SAMPLE);
    }

    #[test]
    fn round_trips_the_real_settings_file_if_present() {
        // The strongest version of the guarantee: the actual 5861-byte file.
        let Ok(Some(profiles)) = crate::profile::discover() else {
            return;
        };
        let Some(active) = profiles.active() else {
            return;
        };
        let Ok(text) = std::fs::read_to_string(&active.settings_path) else {
            return;
        };
        let doc = IniDocument::parse(&text);
        assert_eq!(
            doc.to_string(),
            text,
            "parsing and re-emitting the real file must not change a byte"
        );
    }

    #[test]
    fn reads_values_across_sections() {
        let doc = IniDocument::parse(SAMPLE);
        assert_eq!(doc.get("GENERAL", "Version"), Some("1"));
        assert_eq!(doc.get("DISPLAY_SETTINGS", "WindowMode"), Some("2"));
        assert_eq!(doc.get("DISPLAY_SETTINGS", "ResolutionWidth"), Some("1920"));
        assert_eq!(doc.get("DISPLAY_SETTINGS", "MissingKey"), None);
        assert_eq!(doc.get("NO_SUCH_SECTION", "WindowMode"), None);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let doc = IniDocument::parse(SAMPLE);
        assert_eq!(doc.get("display_settings", "windowmode"), Some("2"));
        assert_eq!(doc.get("Display_Settings", "WINDOWMODE"), Some("2"));
    }

    #[test]
    fn parses_numbers_including_float_formatted_integers() {
        let doc = IniDocument::parse(SAMPLE);
        assert_eq!(doc.get_i64("DISPLAY_SETTINGS", "WindowMode"), Some(2));
        assert_eq!(
            doc.get_f64("DISPLAY_SETTINGS", "RefreshRate"),
            Some(143.912003)
        );
        // Written as a float but conceptually an integer.
        assert_eq!(doc.get_i64("DISPLAY_SETTINGS", "DefaultFOV"), Some(90));
    }

    #[test]
    fn handles_empty_values() {
        let doc = IniDocument::parse(SAMPLE);
        assert_eq!(doc.get("DISPLAY_SETTINGS", "VulkanWhitelistedLayers"), Some(""));
        assert!(doc.contains("DISPLAY_SETTINGS", "VulkanWhitelistedLayers"));
    }

    #[test]
    fn set_changes_exactly_one_line() {
        let mut doc = IniDocument::parse(SAMPLE);
        assert!(doc.set("DISPLAY_SETTINGS", "WindowMode", "0"));
        let after = doc.to_string();

        let before_lines: Vec<&str> = SAMPLE.split("\r\n").collect();
        let after_lines: Vec<&str> = after.split("\r\n").collect();
        assert_eq!(before_lines.len(), after_lines.len(), "line count changed");

        let differing: Vec<usize> = before_lines
            .iter()
            .zip(&after_lines)
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(differing.len(), 1, "expected exactly one changed line");
        assert_eq!(after_lines[differing[0]], "WindowMode=0");
    }

    #[test]
    fn set_preserves_crlf_comments_and_formatting() {
        let mut doc = IniDocument::parse(SAMPLE);
        doc.set("DISPLAY_SETTINGS", "MaxGPUBufferedFrame", "0");
        let after = doc.to_string();

        assert_eq!(after.matches("\r\n").count(), SAMPLE.matches("\r\n").count());
        assert!(after.contains(";WindowMode => 0 fullscreen"), "comment lost");
        // Untouched float formatting must not be normalised.
        assert!(after.contains("DefaultFOV=90.000000"));
        assert!(after.contains("RefreshRate=143.912003"));
        assert!(!after.replace("\r\n", "").contains('\n'), "bare LF introduced");
    }

    #[test]
    fn set_refuses_to_invent_missing_keys() {
        let mut doc = IniDocument::parse(SAMPLE);
        assert!(!doc.set("DISPLAY_SETTINGS", "NotARealSetting", "1"));
        assert!(!doc.set("MADE_UP_SECTION", "WindowMode", "1"));
        assert_eq!(doc.to_string(), SAMPLE, "a failed set must change nothing");
    }

    #[test]
    fn same_key_in_two_sections_is_addressed_independently() {
        let text = "[A]\r\nValue=1\r\n\r\n[B]\r\nValue=2\r\n";
        let mut doc = IniDocument::parse(text);
        assert_eq!(doc.get("A", "Value"), Some("1"));
        assert_eq!(doc.get("B", "Value"), Some("2"));

        doc.set("B", "Value", "99");
        assert_eq!(doc.get("A", "Value"), Some("1"), "wrong section edited");
        assert_eq!(doc.get("B", "Value"), Some("99"));
    }

    #[test]
    fn tolerates_spaces_around_the_equals_sign() {
        let text = "[S]\r\nKey  =  value  \r\n";
        let mut doc = IniDocument::parse(text);
        assert_eq!(doc.get("S", "Key"), Some("value"));

        doc.set("S", "Key", "other");
        // Surrounding whitespace is part of the line, not the value.
        assert_eq!(doc.to_string(), "[S]\r\nKey  =  other  \r\n");
    }

    #[test]
    fn lf_only_files_stay_lf() {
        let text = "[S]\nKey=1\n";
        let mut doc = IniDocument::parse(text);
        assert_eq!(doc.line_ending(), "\n");
        doc.set("S", "Key", "2");
        assert_eq!(doc.to_string(), "[S]\nKey=2\n");
    }

    #[test]
    fn file_without_trailing_newline_round_trips() {
        let text = "[S]\r\nKey=1";
        let doc = IniDocument::parse(text);
        assert_eq!(doc.to_string(), text);
    }

    #[test]
    fn comment_lines_are_not_treated_as_keys() {
        let doc = IniDocument::parse(SAMPLE);
        // The comment contains "WindowMode =>" but must not shadow the real key.
        assert_eq!(doc.get("DISPLAY_SETTINGS", "WindowMode"), Some("2"));
        assert!(!doc.contains("DISPLAY_SETTINGS", ";WindowMode"));
    }

    #[test]
    fn entries_lists_every_setting_in_order() {
        let entries = IniDocument::parse(SAMPLE).entries();
        assert_eq!(entries[0], ("GENERAL".into(), "Version".into(), "1".into()));
        assert!(entries
            .iter()
            .any(|(s, k, v)| s == "DISPLAY_SETTINGS" && k == "WindowMode" && v == "2"));
        // Comments contribute no entries.
        assert!(entries.iter().all(|(_, k, _)| !k.starts_with(';')));
    }

    #[test]
    fn sections_are_listed() {
        let s = IniDocument::parse(SAMPLE).sections();
        assert_eq!(s, vec!["GENERAL", "DISPLAY_SETTINGS"]);
    }
}
