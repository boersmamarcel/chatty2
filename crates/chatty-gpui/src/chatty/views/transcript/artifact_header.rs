//! What the artifact viewer's header should offer for a given artifact.
//!
//! Split out as pure functions over the artifact's kind so the header can be
//! tested by what it *offers* rather than by what it paints (AGE-181). The
//! rule both functions enforce: only offer a choice that exists for this
//! artifact and that does something different from the other choices.

use std::path::Path;

use super::artifact_kind::{is_code_artifact_path, is_markdown_artifact_path};

/// A tab the header shows, and the view index it selects.
///
/// `index` is the viewer's internal numbering (0 = primary body, 1 = source,
/// 2 = diff) and is unchanged by which tabs are visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArtifactTabSpec {
    pub label: &'static str,
    pub index: usize,
}

/// Which of the artifact's two possible payloads to copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactCopyKind {
    /// The file's text as it is on disk.
    Source,
    /// The rendered text, for kinds where that differs from the source.
    Rendered,
}

/// What the copy control should be for this artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactCopy {
    /// Nothing to copy — a PDF, image, chart or browser artifact has no text,
    /// and the old unconditional control silently wrote `""` to the clipboard.
    Hidden,
    /// One button, copies the file's text. No caret: there is only one payload.
    Source,
    /// Button plus a menu, for the two artifact kinds where "source" and
    /// "rendered" really are different text.
    Menu,
}

/// The kinds of artifact the header treats differently.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactHeaderKind {
    /// No text body: PDF, image, chart, browser view.
    Opaque,
    /// Markdown — rendered and source differ.
    Markdown,
    /// Tabular — table and source differ.
    Tabular,
    /// Code, or anything else shown in the editor: the primary view *is* the
    /// source, so a second tab showing the same editor is not a choice.
    Code,
}

impl ArtifactHeaderKind {
    /// Classify from the flags the viewer already computes.
    pub fn resolve(path: Option<&Path>, is_tabular: bool, is_opaque: bool) -> Self {
        if is_opaque {
            return Self::Opaque;
        }
        if is_tabular {
            return Self::Tabular;
        }
        match path {
            Some(path) if is_markdown_artifact_path(path) => Self::Markdown,
            // Code and unknown types both land in the editor, so they get the
            // same treatment. "Preview" for an unknown type duplicated
            // "Source" exactly the way "Code" did.
            Some(path) if is_code_artifact_path(path) => Self::Code,
            _ => Self::Code,
        }
    }

    /// Label for the primary view.
    fn primary_label(self) -> &'static str {
        match self {
            Self::Tabular => "Table",
            Self::Markdown => "Rendered",
            Self::Code | Self::Opaque => "Source",
        }
    }

    /// Whether the primary view renders something other than the source
    /// editor. When it does not, offering both is offering the same thing
    /// twice.
    fn primary_differs_from_source(self) -> bool {
        matches!(self, Self::Markdown | Self::Tabular)
    }
}

/// Tabs to show. An empty result means: render the body with no tab bar.
pub fn artifact_header_tabs(kind: ArtifactHeaderKind, has_diff: bool) -> Vec<ArtifactTabSpec> {
    let mut tabs = vec![ArtifactTabSpec {
        label: kind.primary_label(),
        index: 0,
    }];
    if kind.primary_differs_from_source() {
        tabs.push(ArtifactTabSpec {
            label: "Source",
            index: 1,
        });
    }
    if has_diff {
        tabs.push(ArtifactTabSpec {
            label: "Diff",
            index: 2,
        });
    }
    // One tab is not a choice — show the body bare.
    if tabs.len() < 2 { Vec::new() } else { tabs }
}

/// The copy control for this artifact.
pub fn artifact_copy_control(kind: ArtifactHeaderKind) -> ArtifactCopy {
    match kind {
        ArtifactHeaderKind::Opaque => ArtifactCopy::Hidden,
        ArtifactHeaderKind::Markdown | ArtifactHeaderKind::Tabular => ArtifactCopy::Menu,
        ArtifactHeaderKind::Code => ArtifactCopy::Source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn kind_for(path: &str) -> ArtifactHeaderKind {
        ArtifactHeaderKind::resolve(Some(&PathBuf::from(path)), false, false)
    }

    /// The reported case: `Code` and `Source` rendered the same editor on the
    /// same entity, so tabbing between them did nothing visible.
    #[test]
    fn code_file_without_a_diff_has_no_tab_bar() {
        assert!(artifact_header_tabs(kind_for("form.html"), false).is_empty());
        assert!(artifact_header_tabs(kind_for("main.rs"), false).is_empty());
    }

    /// `Preview` for an unknown type had exactly the same duplication.
    #[test]
    fn unknown_type_without_a_diff_has_no_tab_bar() {
        assert!(artifact_header_tabs(kind_for("notes.weird"), false).is_empty());
    }

    #[test]
    fn code_file_with_a_diff_offers_source_and_diff() {
        let tabs = artifact_header_tabs(kind_for("form.html"), true);
        let labels: Vec<_> = tabs.iter().map(|t| t.label).collect();
        assert_eq!(labels, vec!["Source", "Diff"]);
        // The diff keeps its internal index whatever else is on screen.
        assert_eq!(tabs[1].index, 2);
    }

    #[test]
    fn markdown_still_offers_rendered_and_source() {
        let tabs = artifact_header_tabs(kind_for("README.md"), false);
        let labels: Vec<_> = tabs.iter().map(|t| t.label).collect();
        assert_eq!(labels, vec!["Rendered", "Source"]);
    }

    #[test]
    fn tabular_still_offers_table_and_source() {
        let kind = ArtifactHeaderKind::resolve(Some(&PathBuf::from("rows.csv")), true, false);
        let labels: Vec<_> = artifact_header_tabs(kind, false)
            .iter()
            .map(|t| t.label)
            .collect();
        assert_eq!(labels, vec!["Table", "Source"]);
    }

    /// Copy on a PDF or an image wrote an empty string to the clipboard,
    /// because `open()` clears the text for those kinds.
    #[test]
    fn copy_is_hidden_for_artifacts_with_no_text() {
        let kind = ArtifactHeaderKind::resolve(Some(&PathBuf::from("report.pdf")), false, true);
        assert_eq!(artifact_copy_control(kind), ArtifactCopy::Hidden);
    }

    /// Three menu items, one outcome. A code file gets one button instead.
    #[test]
    fn code_file_gets_a_single_copy_button() {
        assert_eq!(artifact_copy_control(kind_for("form.html")), ArtifactCopy::Source);
    }

    #[test]
    fn only_kinds_with_two_payloads_keep_the_menu() {
        assert_eq!(artifact_copy_control(kind_for("README.md")), ArtifactCopy::Menu);
        let tabular = ArtifactHeaderKind::resolve(Some(&PathBuf::from("rows.csv")), true, false);
        assert_eq!(artifact_copy_control(tabular), ArtifactCopy::Menu);
    }

    /// Every tab the header offers must select a distinct view.
    #[test]
    fn offered_tabs_are_always_distinct_views() {
        for path in ["form.html", "README.md", "rows.csv", "main.rs", "x.weird"] {
            for has_diff in [false, true] {
                let is_tabular = path.ends_with(".csv");
                let kind =
                    ArtifactHeaderKind::resolve(Some(&PathBuf::from(path)), is_tabular, false);
                let tabs = artifact_header_tabs(kind, has_diff);
                let mut indices: Vec<_> = tabs.iter().map(|t| t.index).collect();
                let before = indices.len();
                indices.sort_unstable();
                indices.dedup();
                assert_eq!(before, indices.len(), "{path} (diff={has_diff}) repeats a view");
            }
        }
    }
}
