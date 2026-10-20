// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Capcon contributors

//! Optional caption text cleanup, shared by cloud and on-device transcription.

use serde::{Deserialize, Serialize};
use unicode_categories::UnicodeCategories;

use crate::transcribe::Segment;

/// Readability options for newly generated captions. They never edit the audio.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CaptionCleanup {
    /// Remove sentence punctuation, preserving word hyphens and numeric notation.
    pub strip_punctuation: bool,
    /// Omit hesitation sounds and recognized bracketed non-speech annotations.
    pub clean_speech: bool,
}

impl Default for CaptionCleanup {
    fn default() -> Self {
        Self {
            strip_punctuation: true,
            clean_speech: true,
        }
    }
}

impl CaptionCleanup {
    /// Clean caption text after phrase segmentation, keeping every surviving
    /// phrase's original timing and dropping phrases made empty by cleanup.
    pub fn apply(self, segments: Vec<Segment>) -> Vec<Segment> {
        segments
            .into_iter()
            .filter_map(|mut segment| {
                segment.text = self.text(&segment.text);
                (!segment.text.is_empty()).then_some(segment)
            })
            .collect()
    }

    /// Clean a phrase without guessing whether ordinary words are filler.
    pub fn text(self, text: &str) -> String {
        if !self.clean_speech && !self.strip_punctuation {
            return text.to_owned();
        }
        let mut text = if self.clean_speech {
            let annotated = without_sound_annotations(text);
            let words: Vec<_> = annotated.split_whitespace().collect();
            words
                .iter()
                .enumerate()
                .filter_map(|(index, word)| {
                    let unit = word
                        .trim_matches(|c: char| c.is_punctuation())
                        .to_lowercase()
                        == "мм"
                        && index > 0
                        && words[index - 1]
                            .trim_end_matches(|c: char| c.is_punctuation())
                            .ends_with(|c: char| c.is_ascii_digit());
                    (!hesitation(word) || unit).then_some(*word)
                })
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            text.to_owned()
        };
        if self.strip_punctuation {
            text = without_punctuation(&text);
        }
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    }
}

fn hesitation(word: &str) -> bool {
    let word = word.trim_matches(|c: char| c.is_punctuation());
    // Hyphens within an elongated sound (э-э-э, м-м-м) are conventional.
    // Other internal punctuation must not join unrelated words into a filler.
    let letters: String = word
        .chars()
        .filter(|c| !matches!(c, '-' | '‐' | '‑'))
        .flat_map(char::to_lowercase)
        .collect();
    let count = letters.chars().count();
    (count > 0 && letters.chars().all(|c| c == 'э'))
        || (count >= 2 && letters.chars().all(|c| c == 'м'))
        || (count >= 3 && letters.chars().all(|c| c == 'а'))
        || (letters.starts_with('э') && letters["э".len()..].chars().all(|c| c == 'м'))
        || matches!(letters.as_str(), "um" | "uh" | "erm" | "uhm")
        || (letters.starts_with('u')
            && count >= 3
            && letters.chars().all(|c| matches!(c, 'u' | 'm' | 'h')))
}

fn without_sound_annotations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some((index, open)) = rest.char_indices().find(|(_, c)| matches!(c, '[' | '(')) {
        out.push_str(&rest[..index]);
        rest = &rest[index + open.len_utf8()..];
        let close = if open == '[' { ']' } else { ')' };
        if let Some(end) = rest.find(close) {
            if sound_annotation(&rest[..end]) {
                out.push(' ');
            } else {
                out.push(open);
                out.push_str(&rest[..end]);
                out.push(close);
            }
            rest = &rest[end + close.len_utf8()..];
        } else {
            out.push(open);
            break;
        }
    }
    out.push_str(rest);
    out
}

fn sound_annotation(text: &str) -> bool {
    let label = text
        .to_lowercase()
        .replace(['_', '-'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    matches!(
        label.as_str(),
        "вдох"
            | "выдох"
            | "вздох"
            | "вдохи"
            | "вздыхает"
            | "дыхание"
            | "глубокий вдох"
            | "глубокий выдох"
            | "тяжёлый вздох"
            | "тяжелый вздох"
            | "смех"
            | "смеётся"
            | "смеется"
            | "кашель"
            | "кашляет"
            | "пауза"
            | "тишина"
            | "музыка"
            | "аплодисменты"
            | "шум"
            | "breath"
            | "breathing"
            | "inhale"
            | "inhales"
            | "exhale"
            | "exhales"
            | "deep breath"
            | "inhales deeply"
            | "exhales deeply"
            | "sigh"
            | "sighs"
            | "sighing"
            | "laughter"
            | "laughing"
            | "laughs"
            | "cough"
            | "coughs"
            | "coughing"
            | "music"
            | "background music"
            | "applause"
            | "silence"
            | "blank audio"
            | "no speech"
            | "noise"
    )
}

fn without_punctuation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut previous: Option<char> = None;
    while let Some(ch) = chars.next() {
        let next = chars.peek().copied();
        let between_letters =
            previous.is_some_and(char::is_alphabetic) && next.is_some_and(char::is_alphabetic);
        let between_digits = previous.is_some_and(|c| c.is_ascii_digit())
            && next.is_some_and(|c| c.is_ascii_digit());
        let lexical = between_letters && matches!(ch, '-' | '‐' | '‑' | '\'' | '’');
        let numeric = (between_digits && matches!(ch, '.' | ',' | ':' | '/' | '-'))
            || (ch == '-'
                && next.is_some_and(|c| c.is_ascii_digit())
                && previous.is_none_or(char::is_whitespace))
            || (ch == '%' && previous.is_some_and(|c| c.is_ascii_digit()));
        if !ch.is_punctuation() || lexical || numeric {
            out.push(ch);
        } else if previous.is_some_and(char::is_alphanumeric)
            && next.is_some_and(char::is_alphanumeric)
        {
            out.push(' ');
        }
        previous = Some(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_russian_and_english_hesitations_and_sound_labels() {
        assert_eq!(CaptionCleanup::default().text(
            "Ээээ, привет! [вдох] Э-э-э… ммм, (выдох) эмм, начинаем. Um, uh, uhhh [sighs] let's go!"
        ), "привет начинаем let's go");
    }

    #[test]
    fn keeps_ordinary_words_numbers_word_hyphens_and_meaningful_parentheses() {
        assert_eq!(CaptionCleanup::default().text(
            "А ну вот, мама: м, эмаль, эмоции, по-русски — 3,5 и -12.5, 20%, 10:30 (важное уточнение)."
        ), "А ну вот мама м эмаль эмоции по-русски 3,5 и -12.5 20% 10:30 важное уточнение");
    }

    #[test]
    fn punctuation_options_are_independent_and_unicode_aware() {
        let punctuation_only = CaptionCleanup {
            clean_speech: false,
            strip_punctuation: true,
        };
        assert_eq!(
            punctuation_only.text("«Ммм…» Привет,мир！ Really? Yes;好。再见！"),
            "Ммм Привет мир Really Yes 好 再见"
        );
        let speech_only = CaptionCleanup {
            clean_speech: true,
            strip_punctuation: false,
        };
        assert_eq!(speech_only.text("Эээ, привет, мир! (вдох)"), "привет, мир!");
        let neither = CaptionCleanup {
            clean_speech: false,
            strip_punctuation: false,
        };
        assert_eq!(
            neither.text("Эээ,\n  привет! (вдох)"),
            "Эээ,\n  привет! (вдох)"
        );
    }

    #[test]
    fn preserves_surviving_cue_timing_and_drops_empty_cues() {
        let segments = CaptionCleanup::default().apply(vec![
            Segment {
                start: 0.5,
                end: 1.0,
                text: "Эээ… (вдох)".into(),
            },
            Segment {
                start: 1.2,
                end: 3.4,
                text: "Ммм, привет, мир!".into(),
            },
            Segment {
                start: 4.0,
                end: 4.2,
                text: "…".into(),
            },
        ]);
        assert_eq!(
            segments,
            vec![Segment {
                start: 1.2,
                end: 3.4,
                text: "привет мир".into()
            }]
        );
    }

    #[test]
    fn millimetres_after_a_number_are_not_a_hesitation() {
        assert_eq!(
            CaptionCleanup::default().text("Мм, размер 25 мм, толщина 3,5 ММ."),
            "размер 25 мм толщина 3,5 ММ"
        );
    }

    #[test]
    fn unmatched_annotations_and_non_sound_asides_keep_their_text() {
        let options = CaptionCleanup {
            clean_speech: true,
            strip_punctuation: false,
        };
        assert_eq!(
            options.text("Текст (важно) [пример] [вдох"),
            "Текст (важно) [пример] [вдох"
        );
        assert_eq!(
            options.text("(deep breath) Текст [BLANK_AUDIO] дальше"),
            "Текст дальше"
        );
    }

    #[test]
    fn preference_defaults_and_explicit_disabling_survive_serialization() {
        assert_eq!(
            serde_json::from_str::<CaptionCleanup>("{}").unwrap(),
            CaptionCleanup::default()
        );
        let off = CaptionCleanup {
            clean_speech: false,
            strip_punctuation: false,
        };
        assert_eq!(
            serde_json::from_str::<CaptionCleanup>(&serde_json::to_string(&off).unwrap()).unwrap(),
            off
        );
    }
}
