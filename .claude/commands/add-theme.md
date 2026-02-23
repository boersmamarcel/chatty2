Create a new color theme for Chatty.

The user wants to create a theme called: $ARGUMENTS

## Instructions

1. Create a new JSON file in the `themes/` directory named after the theme (lowercase, hyphens for spaces, e.g., `themes/my-theme.json`)

2. Follow the exact schema used by existing themes. Use `themes/catppuccin.json` as a reference for the full structure.

3. The theme JSON must include:
   - `$schema`: `"https://github.com/longbridge/gpui-component/raw/refs/heads/main/.theme-schema.json"`
   - `name`: Display name of the theme
   - `author`: Creator attribution
   - `themes`: Array with at least one light and/or dark variant

4. Each theme variant needs:
   - `name`: Variant display name (e.g., "My Theme Dark")
   - `mode`: `"light"` or `"dark"`
   - `colors`: Object with UI color tokens (background, foreground, border, accent, primary, secondary, scrollbar, tab, title_bar, base colors, etc.)
   - `highlight`: Object with editor/syntax colors including the `syntax` sub-object for language tokens

5. Required syntax highlight tokens (at minimum):
   - `attribute`, `boolean`, `comment`, `comment.doc`, `constant`, `constructor`
   - `embedded`, `function`, `keyword`, `link_text`, `link_uri`
   - `number`, `string`, `string.escape`, `string.regex`, `string.special`
   - `tag`, `text.literal`, `title`, `type`, `property`, `variable.special`

6. Verify the JSON is valid with proper formatting.

7. The theme will be automatically discovered from the `themes/` directory at startup.
