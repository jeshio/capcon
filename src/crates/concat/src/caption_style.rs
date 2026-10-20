// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Jareer and Concat contributors

//! Styling captions without selecting every cue on their track.

use concat_project::Command;
use concat_project::commands::ClipPatch;
use concat_project::model::{ClipKind, TextStyle, Timeline};

/// Text clips on the selected title's track. A selection spanning tracks
/// is ambiguous, and a locked track cannot be styled.
pub fn track_targets(
    timeline: &Timeline,
    selection: &[String],
    locked: impl Fn(&str) -> bool,
) -> Vec<String> {
    let Some(source) = selection.first().and_then(|id| timeline.clip(id)) else {
        return Vec::new();
    };
    if source.kind != ClipKind::Text
        || locked(&source.track_id)
        || !selection.iter().all(|id| {
            timeline
                .clip(id)
                .is_some_and(|clip| clip.kind == ClipKind::Text && clip.track_id == source.track_id)
        })
    {
        return Vec::new();
    }
    timeline
        .clips
        .iter()
        .filter(|clip| clip.kind == ClipKind::Text && clip.track_id == source.track_id)
        .map(|clip| clip.id.clone())
        .collect()
}

/// Only the look changes: every cue keeps its own words, timing, position
/// and animation. Unchanged clips produce no command or undo entry.
pub fn commands(
    timeline: &Timeline,
    targets: &[String],
    style: &TextStyle,
    locked: impl Fn(&str) -> bool,
) -> Vec<Command> {
    targets
        .iter()
        .filter_map(|id| {
            let clip = timeline.clip(id)?;
            if clip.kind != ClipKind::Text || locked(&clip.track_id) {
                return None;
            }
            let before = clip.text.clone().unwrap_or_default();
            let after = TextStyle {
                content: before.content.clone(),
                ..style.clone()
            };
            (after != before).then(|| Command::UpdateClip {
                clip_id: id.clone(),
                patch: ClipPatch {
                    text: Some(Some(after)),
                    ..ClipPatch::default()
                },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use concat_project::Editor;
    use concat_project::model::{Clip, ClipKey, KeyEase, KeyProperty, Project};

    fn cue(id: &str, track: &str, start: f64) -> Clip {
        let mut clip = Clip::blank(id, track, ClipKind::Text, id, start, 0.75);
        clip.text = Some(TextStyle {
            content: format!("Words for {id}\nSecond line"),
            ..TextStyle::default()
        });
        clip.offset_y = 0.35;
        clip.keys.push(ClipKey {
            property: KeyProperty::Opacity,
            at: 0.0,
            value: 0.5,
            ease: KeyEase::LINEAR,
        });
        clip
    }

    #[test]
    fn a_whole_caption_track_is_styled_in_one_undo_step() {
        let mut project = Project::default();
        let track = project.active().tracks[0].id.clone();
        let timeline = std::sync::Arc::make_mut(&mut project.timelines[0]);
        for index in 0..200 {
            timeline
                .clips
                .push(cue(&format!("c{index}"), &track, index as f64).into());
        }
        // A title on another track and a picture on the captions' track.
        timeline.clips.push(cue("other-title", "other", 0.0).into());
        timeline
            .clips
            .push(Clip::blank("video", &track, ClipKind::Video, "video", 0.0, 200.0).into());
        let targets = track_targets(timeline, &["c0".into()], |_| false);
        assert_eq!(targets.len(), 200);
        let style = TextStyle {
            content: "These words must never replace the cues".into(),
            font_family: "My Font".into(),
            font_size: 0.07,
            color: "#ffcc00".into(),
            stroke_width: 0.01,
            background: "#00000080".into(),
            ..TextStyle::default()
        };
        let mut commands = commands(timeline, &targets, &style, |_| false);
        // The project's update command follows the words when naming a
        // title; the other clip fields must stay exactly as they were.
        let before = project.clone();
        concat_project::commands::apply(
            &mut project,
            &mut Default::default(),
            Command::Batch {
                commands: commands.clone(),
            },
        )
        .expect("style applies");
        for clip in &before.active().clips {
            let mut expected = clip.as_ref().clone();
            if targets.contains(&clip.id) {
                let words = clip.text.as_ref().unwrap().content.clone();
                expected.name = words.lines().next().unwrap().to_owned();
                expected.text = Some(TextStyle {
                    content: words,
                    ..style.clone()
                });
            }
            assert_eq!(project.active().clip(&clip.id), Some(&expected));
        }
        assert!(self::commands(project.active(), &targets, &style, |_| false).is_empty());

        // Exercise the actual editor's batch history, not an inverse patch.
        let mut editor = Editor::new();
        for index in 0..3 {
            editor
                .apply(Command::AddTextClip {
                    track_id: None,
                    start: index as f64,
                    duration: Some(0.75),
                    style: Some(TextStyle {
                        content: format!("Cue {index}"),
                        ..TextStyle::default()
                    }),
                    offset_y: None,
                    above: true,
                })
                .unwrap();
        }
        let before = editor.project().clone();
        let ids = before
            .active()
            .clips
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>();
        commands = self::commands(before.active(), &ids, &style, |_| false);
        editor.apply(Command::Batch { commands }).unwrap();
        let after = editor.project().clone();
        assert_ne!(before, after);
        assert!(editor.undo());
        assert_eq!(editor.project(), &before);
        assert!(editor.redo());
        assert_eq!(editor.project(), &after);
    }

    #[test]
    fn ambiguous_selections_and_locked_tracks_are_not_changed() {
        let mut project = Project::default();
        let timeline = std::sync::Arc::make_mut(&mut project.timelines[0]);
        timeline.clips = vec![
            cue("a", "captions", 0.0).into(),
            cue("b", "titles", 0.0).into(),
        ];
        assert!(track_targets(timeline, &["a".into(), "b".into()], |_| false).is_empty());
        assert!(track_targets(timeline, &["missing".into()], |_| false).is_empty());
        assert!(track_targets(timeline, &[], |_| false).is_empty());
        assert!(track_targets(timeline, &["a".into()], |t| t == "captions").is_empty());
        assert!(commands(timeline, &["a".into()], &TextStyle::default(), |_| true).is_empty());
    }

    #[test]
    fn a_preset_preserves_each_selected_cues_words_and_ignores_non_text() {
        let mut project = Project::default();
        let timeline = std::sync::Arc::make_mut(&mut project.timelines[0]);
        timeline.clips = vec![
            cue("a", "captions", 0.0).into(),
            Clip::blank("video", "captions", ClipKind::Video, "v", 0.0, 1.0).into(),
        ];
        assert!(track_targets(timeline, &["video".into()], |_| false).is_empty());
        let style = TextStyle {
            font_size: 0.12,
            ..TextStyle::default()
        };
        let edits = commands(
            timeline,
            &["a".into(), "video".into(), "gone".into()],
            &style,
            |_| false,
        );
        assert_eq!(edits.len(), 1);
        let Command::UpdateClip { patch, .. } = &edits[0] else {
            panic!("one text update");
        };
        assert_eq!(
            patch.text.as_ref().unwrap().as_ref().unwrap().content,
            "Words for a\nSecond line"
        );
    }
}
