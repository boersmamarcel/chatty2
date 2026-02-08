use gpui::*;
use gpui_component::ActiveTheme;
use gpui_component::{Icon, Sizable};
use gpui_component::button::{Button, ButtonVariants};
use crate::assets::CustomIcon;
use super::syntax_highlighter::{self, HighlightedSpan};
use once_cell::sync::Lazy;
use std::sync::RwLock;
use std::collections::HashMap;

// Global cache for highlighted code spans
type HighlightCache = HashMap<(String, Option<String>), Vec<HighlightedSpan>>;
static HIGHLIGHT_CACHE: Lazy<RwLock<HighlightCache>> = Lazy::new(|| RwLock::new(HashMap::new()));

/// A code block component with syntax highlighting and a copy button
#[derive(IntoElement, Clone)]
pub struct CodeBlockComponent {
    language: Option<String>,
    code: String,
    block_index: usize,
}

impl CodeBlockComponent {
    pub fn new(language: Option<String>, code: String, block_index: usize) -> Self {
        Self {
            language,
            code,
            block_index,
        }
    }
    
    /// Get highlighted spans from cache or compute them
    fn get_highlighted_spans(&self, cx: &App) -> Vec<HighlightedSpan> {
        let cache_key = (self.code.clone(), self.language.clone());
        
        // Try to get from cache first
        if let Ok(cache) = HIGHLIGHT_CACHE.read() {
            if let Some(cached_spans) = cache.get(&cache_key) {
                return cached_spans.clone();
            }
        }
        
        // Cache miss - compute highlights
        let spans = syntax_highlighter::highlight_code(
            &self.code,
            self.language.as_deref(),
            cx,
        );
        
        // Store in cache
        if let Ok(mut cache) = HIGHLIGHT_CACHE.write() {
            cache.insert(cache_key, spans.clone());
        }
        
        spans
    }
}

impl RenderOnce for CodeBlockComponent {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let bg_color = cx.theme().muted;
        let border_color = cx.theme().border;

        // Get highlighted spans (cached or computed)
        let highlighted_spans = self.get_highlighted_spans(cx);

        div()
            .relative() // For absolute positioning of copy button
            .bg(bg_color)
            .border_1()
            .border_color(border_color)
            .rounded_md()
            .mb_3()
            .p_3()
            .child(
                div()
                    .relative()
                    .font_family("monospace")
                    .text_size(px(13.0))
                    .line_height(relative(1.5))
                    // Render code line by line to preserve formatting
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_0()
                            .children(
                                // Group spans by lines
                                self.render_lines(highlighted_spans)
                            )
                    )
                    // Copy button (top-right overlay)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .child(
                                Button::new(ElementId::Name(
                                    format!("copy-code-btn-{}", self.block_index).into(),
                                ))
                                .ghost()
                                .xsmall()
                                .icon(Icon::new(CustomIcon::Copy))
                                .tooltip("Copy code")
                                .on_click({
                                    let code = self.code.clone();
                                    move |_event, _window, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            code.clone(),
                                        ));
                                    }
                                })
                            )
                    )
            )
    }
}

impl CodeBlockComponent {
    /// Render highlighted spans as lines (Phase 2: simplified DOM structure)
    fn render_lines(&self, spans: Vec<HighlightedSpan>) -> Vec<Div> {
        let mut lines = Vec::new();
        let mut current_line_parts: Vec<(String, Hsla)> = Vec::new();
        
        for span in spans {
            // Split span by newlines
            let parts: Vec<&str> = span.text.split('\n').collect();
            
            for (i, part) in parts.iter().enumerate() {
                if i > 0 {
                    // We hit a newline - flush current line
                    if !current_line_parts.is_empty() {
                        lines.push(self.render_single_line(current_line_parts.drain(..).collect()));
                    } else {
                        // Empty line - still need a div for spacing
                        lines.push(div().flex().flex_row().child(""));
                    }
                }
                
                if !part.is_empty() {
                    current_line_parts.push((part.to_string(), span.color));
                }
            }
        }
        
        // Push final line if any
        if !current_line_parts.is_empty() {
            lines.push(self.render_single_line(current_line_parts));
        }
        
        lines
    }
    
    /// Render a single line with multiple colored parts
    /// Phase 2 optimization: combine adjacent parts into fewer elements
    fn render_single_line(&self, parts: Vec<(String, Hsla)>) -> Div {
        let mut line = div().flex().flex_row();
        
        for (text, color) in parts {
            line = line.child(
                div()
                    .text_color(color)
                    .child(text)
            );
        }
        
        line
    }
}
