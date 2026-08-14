use std::ops::Range;

use harness_core::tools::skill::SkillSlashEntry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPickerEntry {
    pub name: String,
    pub description: String,
    pub model_invocable: bool,
}

impl SkillPickerEntry {
    pub fn menu_description(&self) -> String {
        if self.model_invocable {
            self.description.clone()
        } else {
            format!("User only · {}", self.description)
        }
    }
}

impl From<SkillSlashEntry> for SkillPickerEntry {
    fn from(entry: SkillSlashEntry) -> Self {
        Self {
            name: entry.name,
            description: entry.description,
            model_invocable: entry.model_invocable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashToken {
    pub range: Range<usize>,
    pub query: String,
}

#[derive(Debug, Default)]
pub struct SkillPickerState {
    catalog: Vec<SkillPickerEntry>,
    filtered: Vec<usize>,
    active: Option<usize>,
    token: Option<SlashToken>,
    dismissed: Option<(Range<usize>, String)>,
}

impl SkillPickerState {
    pub fn set_catalog(&mut self, catalog: Vec<SkillPickerEntry>) {
        self.catalog = catalog;
        self.refilter();
    }

    pub fn update(&mut self, text: &str, cursor: usize) {
        let token = slash_token(text, cursor);
        let still_dismissed = token.as_ref().is_some_and(|token| {
            self.dismissed.as_ref().is_some_and(|(range, value)| {
                token.range == *range && text.get(range.clone()) == Some(value.as_str())
            })
        });
        if still_dismissed {
            self.token = None;
            self.filtered.clear();
            self.active = None;
            return;
        }

        self.dismissed = None;
        self.token = token;
        self.refilter();
    }

    pub fn is_open(&self) -> bool {
        self.token.is_some()
    }

    pub fn filtered_entries(&self) -> impl Iterator<Item = &SkillPickerEntry> {
        self.filtered
            .iter()
            .filter_map(|index| self.catalog.get(*index))
    }

    pub fn filtered_len(&self) -> usize {
        self.filtered.len()
    }

    pub fn selected_name(&self) -> Option<String> {
        self.selected_entry().map(|entry| entry.name.clone())
    }

    pub fn is_selected(&self, row: usize) -> bool {
        self.active == Some(row)
    }

    pub fn move_active(&mut self, delta: i32) {
        self.active = step_active(self.active, self.filtered.len(), delta);
    }

    pub fn select(&mut self, row: usize) {
        if row < self.filtered.len() {
            self.active = Some(row);
        }
    }

    pub fn dismiss(&mut self, text: &str) -> Option<SlashToken> {
        let token = self.token.clone()?;
        let dismissed = text
            .get(token.range.clone())
            .map(|value| (token.range.clone(), value.to_string()));
        self.token = None;
        self.filtered.clear();
        self.active = None;
        self.dismissed = dismissed;
        Some(token)
    }

    pub fn accept(&mut self) -> Option<(SlashToken, String)> {
        let token = self.token.clone()?;
        let name = self.selected_name()?;
        self.token = None;
        self.filtered.clear();
        self.active = None;
        self.dismissed = None;
        Some((token, name))
    }

    fn selected_entry(&self) -> Option<&SkillPickerEntry> {
        let active = self.active?;
        let catalog_index = *self.filtered.get(active)?;
        self.catalog.get(catalog_index)
    }

    fn refilter(&mut self) {
        let query = self
            .token
            .as_ref()
            .map(|token| token.query.as_str())
            .unwrap_or_default();
        self.filtered = filter_entries(&self.catalog, query);
        self.active = (!self.filtered.is_empty()).then_some(0);
    }
}

pub fn slash_token(text: &str, cursor: usize) -> Option<SlashToken> {
    if cursor == 0 || cursor > text.len() || !text.is_char_boundary(cursor) {
        return None;
    }
    for (index, ch) in text
        .char_indices()
        .filter(|(index, _)| *index < cursor)
        .rev()
    {
        if ch.is_whitespace() {
            return None;
        }
        if ch == '/' && boundary_allows_slash(text, index) {
            return Some(SlashToken {
                range: index..cursor,
                query: text[index + 1..cursor].to_string(),
            });
        }
    }
    None
}

fn boundary_allows_slash(text: &str, index: usize) -> bool {
    let Some(previous) = text[..index].chars().next_back() else {
        return true;
    };
    if previous.is_whitespace() {
        return true;
    }
    if previous.is_alphanumeric() || previous == '_' {
        return false;
    }
    if previous == '/' {
        return false;
    }
    if previous == ':' {
        let before_scheme = text[..index - previous.len_utf8()]
            .chars()
            .next_back()
            .is_some_and(|ch| !ch.is_whitespace());
        return !before_scheme;
    }
    true
}

pub fn filter_entries(entries: &[SkillPickerEntry], query: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.name.starts_with(query))
        .map(|(index, _)| index)
        .collect()
}

pub fn step_active(active: Option<usize>, count: usize, delta: i32) -> Option<usize> {
    if count == 0 {
        return None;
    }
    let Some(current) = active else {
        return Some(0);
    };
    if current >= count {
        return Some(0);
    }
    let next = current as isize + delta as isize;
    Some(next.rem_euclid(count as isize) as usize)
}

#[cfg(test)]
fn insert_entry(text: &str, range: Range<usize>, name: &str) -> (String, usize) {
    let replacement = entry_replacement(name);
    let suffix = text.get(range.end..).unwrap_or_default();
    let mut updated = String::with_capacity(text.len() + replacement.len());
    updated.push_str(&text[..range.start]);
    updated.push_str(&replacement);
    updated.push_str(suffix);
    let cursor = range.start + replacement.len();
    (updated, cursor)
}

pub fn entry_replacement(name: &str) -> String {
    format!("/{name} ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, description: &str, model_invocable: bool) -> SkillPickerEntry {
        SkillPickerEntry {
            name: name.into(),
            description: description.into(),
            model_invocable,
        }
    }

    #[test]
    fn slash_tokens_follow_the_dsh_leading_token_contract() {
        assert_eq!(
            slash_token("/pre", 4),
            Some(SlashToken {
                range: 0..4,
                query: "pre".into()
            })
        );
        assert_eq!(
            slash_token("/pre argument", 4),
            Some(SlashToken {
                range: 0..4,
                query: "pre".into()
            })
        );
        assert_eq!(slash_token("/", 0), None);
        assert_eq!(
            slash_token("use /pre", 8),
            Some(SlashToken {
                range: 4..8,
                query: "pre".into()
            })
        );
        assert_eq!(
            slash_token("(/pre)", 5),
            Some(SlashToken {
                range: 1..5,
                query: "pre".into()
            })
        );
        assert_eq!(slash_token("https://x", 9), None);
        assert_eq!(slash_token("a/b", 3), None);
        assert_eq!(slash_token("/pre", 5), None);
    }

    #[test]
    fn slash_entries_filter_by_name_prefix_and_mark_user_only_skills() {
        let entries = vec![
            entry("prestep", "Run setup.", true),
            entry("private", "Human only.", false),
            entry("other", "Not a prefix.", true),
        ];

        assert_eq!(filter_entries(&entries, "pre"), vec![0]);
        assert_eq!(filter_entries(&entries, "p"), vec![0, 1]);
        assert!(filter_entries(&entries, "PRE").is_empty());
        assert_eq!(entries[1].menu_description(), "User only · Human only.");
        assert_eq!(entries[0].menu_description(), "Run setup.");
    }

    #[test]
    fn slash_navigation_wraps_like_comet() {
        assert_eq!(step_active(None, 3, 1), Some(0));
        assert_eq!(step_active(Some(0), 3, 1), Some(1));
        assert_eq!(step_active(Some(2), 3, 1), Some(0));
        assert_eq!(step_active(Some(0), 3, -1), Some(2));
        assert_eq!(step_active(Some(1), 0, 1), None);
    }

    #[test]
    fn accepting_an_entry_replaces_the_leading_token_and_keeps_the_cursor_at_the_end() {
        assert_eq!(
            insert_entry("/pre argument", 0..4, "prestep"),
            ("/prestep  argument".to_string(), 9)
        );
        assert_eq!(
            insert_entry("/pre", 0..4, "prestep"),
            ("/prestep ".to_string(), 9)
        );
    }

    #[test]
    fn picker_state_tracks_tokens_filtering_and_dismissal() {
        let mut state = SkillPickerState::default();
        state.set_catalog(vec![
            entry("prestep", "Run setup.", true),
            entry("private", "Human only.", false),
        ]);

        state.update("/p", 2);
        assert!(state.is_open());
        assert_eq!(state.selected_name(), Some("prestep".to_string()));

        state.move_active(1);
        assert_eq!(state.selected_name(), Some("private".to_string()));

        state.update("/q", 2);
        assert!(state.is_open());
        assert_eq!(state.selected_name(), None);

        state.update("/p", 2);
        let dismissed = state.dismiss("/p").unwrap();
        assert_eq!(dismissed.range, 0..2);
        assert!(!state.is_open());

        state.update("/p", 2);
        assert!(
            !state.is_open(),
            "an unchanged dismissed token stays closed"
        );

        state.update("/pr", 3);
        assert!(state.is_open());
        let accepted = state.accept().unwrap();
        assert_eq!(accepted.0.range, 0..3);
        assert_eq!(accepted.1, "prestep");
        assert!(!state.is_open());
    }

    #[test]
    fn slash_catalog_entries_convert_into_picker_rows() {
        let entries = vec![harness_core::tools::skill::SkillSlashEntry {
            name: "user-only".into(),
            description: "Human only.".into(),
            model_invocable: false,
        }];
        let converted: Vec<SkillPickerEntry> = entries.into_iter().map(Into::into).collect();

        assert_eq!(
            converted,
            vec![SkillPickerEntry {
                name: "user-only".into(),
                description: "Human only.".into(),
                model_invocable: false,
            }]
        );
    }
}
