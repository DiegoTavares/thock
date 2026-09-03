//! The Day Planner's parsing and layout model: turns a daily note's Markdown
//! checklist into timed/unscheduled plan items and places timed items into
//! Google-Calendar-style columns. Pure functions over strings and minutes —
//! no GPUI — so the whole contract is unit-testable without a window.

/// Resolved `[day_planner]` settings. Parsing of the raw `config.toml` table
/// lives in `vault.rs`; times here are minutes since midnight.
#[derive(Debug, Clone, PartialEq)]
pub struct DayPlannerConfig {
    /// Heading whose section is parsed (matched case-insensitively against
    /// any ATX heading). Empty means the whole note is always parsed.
    pub heading: String,
    /// Top of the grid, in minutes since midnight. The grid auto-expands
    /// earlier when a task starts before this.
    pub day_start: u32,
    /// Bottom of the grid, in minutes since midnight (`1440` = end of day).
    /// The grid auto-expands later when a task ends after this.
    pub day_end: u32,
    /// Duration, in minutes, for tasks written with only a start time.
    pub default_duration: u32,
    pub show_now_indicator: bool,
    /// `[day_planner.sections]` palette-index pins, keyed by the trimmed,
    /// lowercased section name (spec v8 §7.3). Sections not listed here get
    /// the hashed index.
    pub sections: std::collections::HashMap<String, usize>,
}

impl Default for DayPlannerConfig {
    fn default() -> Self {
        Self {
            heading: "Day planner".to_string(),
            day_start: 6 * 60,
            day_end: 24 * 60,
            default_duration: 30,
            show_now_indicator: true,
            sections: std::collections::HashMap::new(),
        }
    }
}

pub const MINUTES_PER_DAY: u32 = 24 * 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemTiming {
    /// Rendered as a block on the grid. `end_min` is always in
    /// `start_min + 1 ..= MINUTES_PER_DAY`.
    Timed { start_min: u32, end_min: u32 },
    /// Rendered as a chip in the unscheduled strip.
    Unscheduled,
}

/// How much of the grid an item claims. A calendar *status* block — Google's
/// focus time or out of office — is a container for hours of the day rather
/// than a thing to do, so it lays out in its own narrow lane instead of
/// splitting the width of every real block it happens to span.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ItemWeight {
    #[default]
    Normal,
    Status,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PlanItem {
    /// 0-based buffer row of the checkbox line, for reveal-on-click.
    pub row: u32,
    pub done: bool,
    /// Which lane the item lays out in, from the sync marker's event kind.
    pub weight: ItemWeight,
    /// Whether the whole label is `~~struck through~~` — a task the user
    /// crossed out rather than ticked. The panel treats it as finished.
    pub struck: bool,
    /// Task text with any leading time token and trailing HTML comment removed.
    pub label: String,
    pub timing: ItemTiming,
    /// The subsection heading (exactly one level deeper than the planner
    /// heading) the item sits under; `None` for root-level items and when the
    /// whole note is parsed without a planner heading (spec v8 §11.1).
    pub section: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct DayPlan {
    pub items: Vec<PlanItem>,
}

impl DayPlan {
    /// The subsection names present in the plan, in first-appearance order.
    pub fn sections(&self) -> Vec<&str> {
        let mut sections: Vec<&str> = Vec::new();
        for item in &self.items {
            if let Some(section) = item.section.as_deref()
                && !sections.contains(&section)
            {
                sections.push(section);
            }
        }
        sections
    }

    pub fn unscheduled_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.timing == ItemTiming::Unscheduled)
            .map(|(index, _)| index)
    }

    pub fn has_timed_items(&self) -> bool {
        self.items
            .iter()
            .any(|item| matches!(item.timing, ItemTiming::Timed { .. }))
    }
}

/// Parses a note's text into the plan model (spec §5). Candidate lines are
/// Markdown checkbox tasks inside the configured heading's section, or the
/// whole note when the heading is absent. Malformed times never drop a task —
/// it just becomes unscheduled with its raw text as the label.
pub fn parse_day_plan(text: &str, config: &DayPlannerConfig) -> DayPlan {
    let lines: Vec<&str> = text.lines().collect();
    let (range, planner_level) = match planner_section(&lines, &config.heading) {
        Some((range, level)) => (range, Some(level)),
        None => (0..lines.len(), None),
    };
    let mut items = Vec::new();
    let mut current_section: Option<String> = None;
    for row in range {
        let Some(line) = lines.get(row) else {
            continue;
        };
        if let Some(planner_level) = planner_level
            && let Some((level, heading_text)) = heading_level_and_text(line)
            // Only headings exactly one level deeper start a subsection;
            // deeper ones belong to the current subsection (spec v8 §11.1).
            && level == planner_level + 1
        {
            current_section = Some(heading_text.to_string());
            continue;
        }
        let Some((done, task_text)) = parse_task_line(line) else {
            continue;
        };
        // The event kind lives in the sync marker, so it is read before the
        // trailing comment comes off.
        let weight = if crate::calendar::line_event_kind(line).is_status() {
            ItemWeight::Status
        } else {
            ItemWeight::Normal
        };
        // A crossed-out task may wrap its time token (`~~09:00 Meeting~~`) or
        // only its text (`09:00 ~~Meeting~~`), so the wrapper comes off on
        // both sides of the time parse — a struck task still belongs on the
        // grid at its hour.
        let (task_text, struck_line) = unwrap_strikethrough(strip_trailing_comment(task_text));
        let (timing, label) = match parse_leading_time(task_text, config.default_duration) {
            Some((start_min, end_min, rest)) => (ItemTiming::Timed { start_min, end_min }, rest),
            None => (ItemTiming::Unscheduled, task_text),
        };
        let (label, struck_label) = unwrap_strikethrough(label);
        items.push(PlanItem {
            row: row as u32,
            done,
            weight,
            struck: struck_line || struck_label,
            label: label.to_string(),
            timing,
            section: current_section.clone(),
        });
    }
    DayPlan { items }
}

/// Trims `text` and, when the whole of it is one `~~strikethrough~~` run,
/// unwraps it — reporting that the task was crossed out. A partly struck text
/// (`Call ~~Bob~~ Alice`) is left as written and is still an open task: only
/// crossing the whole thing out means the user dropped it.
fn unwrap_strikethrough(text: &str) -> (&str, bool) {
    let trimmed = text.trim();
    match crate::markdown_syntax::inline_strikethroughs(trimmed).as_slice() {
        [run] if run.range == (0..trimmed.len()) => (trimmed[run.text.clone()].trim(), true),
        _ => (trimmed, false),
    }
}

/// Removes one trailing HTML comment (`<!-- … -->` at end of line) from a
/// label, plus the whitespace before it. Generic rather than gcal-specific,
/// so a user's own trailing comment is hidden the same way (spec v8 §11.4).
pub fn strip_trailing_comment(label: &str) -> &str {
    let trimmed = label.trim_end();
    if !trimmed.ends_with("-->") {
        return label;
    }
    match trimmed.rfind("<!--") {
        Some(start) if start + 4 <= trimmed.len() - 3 => trimmed[..start].trim_end(),
        _ => label,
    }
}

/// A stable palette slot for a subsection name: FNV-1a over the trimmed,
/// lowercased name, so a section keeps its colour across re-parse, restart,
/// and days (spec v8 §11.2).
pub fn section_palette_index(name: &str, palette_len: usize) -> usize {
    if palette_len == 0 {
        return 0;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.trim().to_lowercase().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    (hash % palette_len as u64) as usize
}

/// The palette slot for a section, honouring a `[day_planner.sections]` pin
/// when it is in range; an out-of-range pin is a logged warning and falls
/// back to the hashed index (spec v8 §11.3).
pub fn section_palette_slot(name: &str, config: &DayPlannerConfig, palette_len: usize) -> usize {
    let key = name.trim().to_lowercase();
    if let Some(&pinned) = config.sections.get(&key) {
        if pinned < palette_len {
            return pinned;
        }
        log::warn!(
            "Thock: [day_planner.sections] pins {name:?} to colour {pinned}, but the \
             theme only has {palette_len}; using the hashed colour"
        );
    }
    section_palette_index(name, palette_len)
}

/// The line range under the first heading matching `heading`
/// (case-insensitively), ending before the next heading of equal or higher
/// level, plus the matched heading's level. `None` when the heading is unset
/// or not found.
pub(crate) fn planner_section(
    lines: &[&str],
    heading: &str,
) -> Option<(std::ops::Range<usize>, usize)> {
    let wanted = heading.trim();
    if wanted.is_empty() {
        return None;
    }
    let wanted = wanted.to_lowercase();
    let (start, level) = lines.iter().enumerate().find_map(|(index, line)| {
        let (level, text) = heading_level_and_text(line)?;
        (text.to_lowercase() == wanted).then_some((index, level))
    })?;
    let end = lines[start + 1..]
        .iter()
        .position(|line| heading_level_and_text(line).is_some_and(|(l, _)| l <= level))
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    Some((start + 1..end, level))
}

pub(crate) fn heading_level_and_text(line: &str) -> Option<(usize, &str)> {
    let trimmed = line.trim_start();
    let after_hashes = trimmed.trim_start_matches('#');
    let level = trimmed.len() - after_hashes.len();
    if level == 0 || level > 6 {
        return None;
    }
    if !after_hashes.is_empty() && !after_hashes.starts_with([' ', '\t']) {
        return None;
    }
    Some((level, after_hashes.trim()))
}

/// Matches `[-*+] \s+ [( |x|X)] \s+ <text>` (spec §5.1), returning the done
/// state and the task text.
fn parse_task_line(line: &str) -> Option<(bool, &str)> {
    let after_bullet = line.trim_start().strip_prefix(['-', '*', '+'])?;
    let after_space = after_bullet.trim_start();
    if after_space.len() == after_bullet.len() {
        return None;
    }
    let mut chars = after_space.strip_prefix('[')?.chars();
    let done = match chars.next()? {
        ' ' => false,
        'x' | 'X' => true,
        _ => return None,
    };
    let after_checkbox = chars.as_str().strip_prefix(']')?;
    let text = after_checkbox.trim_start();
    if text.len() == after_checkbox.len() && !text.is_empty() {
        return None;
    }
    Some((done, text))
}

/// Extracts a leading time token — `HH:MM – HH:MM` (range) or `HH:MM`
/// (start-only) — from the task text (spec §5.3), returning
/// `(start, end, label)`. `None` means the text carries no valid leading
/// time and the task is unscheduled.
fn parse_leading_time(text: &str, default_duration: u32) -> Option<(u32, u32, &str)> {
    let (start_min, after_start) = parse_time_prefix(text)?;
    if let Some((range_end, after_end)) = parse_range_end(after_start)
        && let Some(rest) = label_after_token(after_end)
    {
        // A range whose end is at or before its start (a typo, or a task
        // crossing midnight) is forgiven: it gets the default duration.
        let end_min = if range_end > start_min {
            range_end
        } else {
            (start_min + default_duration).min(MINUTES_PER_DAY)
        };
        return Some((start_min, end_min, rest));
    }
    let rest = label_after_token(after_start)?;
    let end_min = (start_min + default_duration).min(MINUTES_PER_DAY);
    Some((start_min, end_min, rest))
}

/// `H:MM` / `HH:MM`, 24-hour, at the very start of `text`. Returns the
/// minutes since midnight and the remaining text.
pub(crate) fn parse_time_prefix(text: &str) -> Option<(u32, &str)> {
    let bytes = text.as_bytes();
    let mut hour_digits = 0;
    while hour_digits < 2
        && bytes
            .get(hour_digits)
            .is_some_and(|byte| byte.is_ascii_digit())
    {
        hour_digits += 1;
    }
    if hour_digits == 0 || bytes.get(hour_digits) != Some(&b':') {
        return None;
    }
    let minutes_range = hour_digits + 1..hour_digits + 3;
    let minutes_text = text.get(minutes_range.clone())?;
    if !minutes_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let hours: u32 = text.get(..hour_digits)?.parse().ok()?;
    let minutes: u32 = minutes_text.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some((hours * 60 + minutes, text.get(minutes_range.end..)?))
}

/// A range separator (`–`, `—`, `-`, or `to`, optional surrounding spaces)
/// followed by a valid time. `None` leaves the caller on the start-only path.
pub(crate) fn parse_range_end(text: &str) -> Option<(u32, &str)> {
    let trimmed = text.trim_start();
    let after_separator = trimmed
        .strip_prefix('–')
        .or_else(|| trimmed.strip_prefix('—'))
        .or_else(|| trimmed.strip_prefix('-'))
        .or_else(|| trimmed.strip_prefix("to"))?
        .trim_start();
    // `24:00` is accepted as a range *end* (a task running to midnight),
    // though never as a start.
    if let Some(rest) = after_separator.strip_prefix("24:00") {
        return Some((MINUTES_PER_DAY, rest));
    }
    parse_time_prefix(after_separator)
}

/// After a time token there must be at least one whitespace character before
/// the label, or nothing at all (empty label). Anything else means the
/// "time" was malformed, and the whole text is treated as unscheduled.
fn label_after_token(after_token: &str) -> Option<&str> {
    if after_token.is_empty() {
        return Some("");
    }
    let rest = after_token.trim_start();
    (rest.len() < after_token.len()).then_some(rest)
}

/// Parses a `[day_planner].day_start` / `day_end` value: `HH:MM`, with
/// `24:00` allowed as end-of-day.
pub fn parse_grid_bound(text: &str) -> Option<u32> {
    let trimmed = text.trim();
    if trimmed == "24:00" {
        return Some(MINUTES_PER_DAY);
    }
    let (minutes, rest) = parse_time_prefix(trimmed)?;
    rest.is_empty().then_some(minutes)
}

/// The grid's vertical extent in minutes: `day_start`/`day_end` expanded to
/// fit every timed task, rounded outward to whole hours (spec §7.2).
pub fn grid_bounds(plan: &DayPlan, config: &DayPlannerConfig) -> (u32, u32) {
    let mut start = config.day_start.min(config.day_end.saturating_sub(60));
    let mut end = config.day_end;
    for item in &plan.items {
        if let ItemTiming::Timed { start_min, end_min } = item.timing {
            start = start.min(start_min);
            end = end.max(end_min);
        }
    }
    (start / 60 * 60, end.div_ceil(60) * 60)
}

/// A timed item placed on the grid. `column` / `column_count` describe the
/// side-by-side slot within the block's overlap cluster (spec §7.6);
/// non-overlapping blocks get `column_count == 1` (full width).
#[derive(Debug, Clone, PartialEq)]
pub struct PlacedBlock {
    /// Index into `DayPlan::items`.
    pub item_index: usize,
    pub start_min: u32,
    pub end_min: u32,
    pub column: usize,
    pub column_count: usize,
}

/// Places every timed item into overlap clusters and columns. The two lanes
/// are laid out independently, so a focus-time block spanning the day never
/// costs a real meeting half its width; `column`/`column_count` are relative
/// to the item's own lane.
///
/// `min_visual_minutes` is the minute-equivalent of the minimum block height:
/// blocks too short to render at true scale still occupy that much vertical
/// space, so overlap is computed against this visual extent — otherwise two
/// back-to-back 5-minute tasks would draw on top of each other.
pub fn layout_blocks(plan: &DayPlan, min_visual_minutes: u32) -> Vec<PlacedBlock> {
    let mut blocks = layout_lane(plan, ItemWeight::Normal, min_visual_minutes);
    blocks.extend(layout_lane(plan, ItemWeight::Status, min_visual_minutes));
    blocks
}

/// Whether the plan has a timed item in the status lane — the panel only
/// reserves the lane's width when something is in it.
pub fn has_status_blocks(plan: &DayPlan) -> bool {
    plan.items.iter().any(|item| {
        item.weight == ItemWeight::Status && matches!(item.timing, ItemTiming::Timed { .. })
    })
}

fn layout_lane(plan: &DayPlan, weight: ItemWeight, min_visual_minutes: u32) -> Vec<PlacedBlock> {
    let mut blocks: Vec<PlacedBlock> = plan
        .items
        .iter()
        .enumerate()
        .filter(|(_, item)| item.weight == weight)
        .filter_map(|(item_index, item)| match item.timing {
            ItemTiming::Timed { start_min, end_min } => Some(PlacedBlock {
                item_index,
                start_min,
                end_min,
                column: 0,
                column_count: 1,
            }),
            ItemTiming::Unscheduled => None,
        })
        .collect();
    blocks.sort_by_key(|block| (block.start_min, block.end_min));

    let visual_end = |block: &PlacedBlock| block.end_min.max(block.start_min + min_visual_minutes);

    let mut cluster_start = 0;
    let mut cluster_max_end = 0;
    for index in 0..=blocks.len() {
        let cluster_ended = match blocks.get(index) {
            Some(block) => index > cluster_start && block.start_min >= cluster_max_end,
            None => index > cluster_start,
        };
        if cluster_ended {
            assign_columns(&mut blocks[cluster_start..index], min_visual_minutes);
            cluster_start = index;
            cluster_max_end = 0;
        }
        if let Some(block) = blocks.get(index) {
            cluster_max_end = cluster_max_end.max(visual_end(block));
        }
    }
    blocks
}

/// Greedy interval coloring: each block takes the leftmost column whose last
/// block ends at or before this block's start.
fn assign_columns(cluster: &mut [PlacedBlock], min_visual_minutes: u32) {
    let mut column_ends: Vec<u32> = Vec::new();
    for block in cluster.iter_mut() {
        let visual_end = block.end_min.max(block.start_min + min_visual_minutes);
        match column_ends.iter().position(|&end| end <= block.start_min) {
            Some(column) => {
                column_ends[column] = visual_end;
                block.column = column;
            }
            None => {
                block.column = column_ends.len();
                column_ends.push(visual_end);
            }
        }
    }
    let column_count = column_ends.len().max(1);
    for block in cluster.iter_mut() {
        block.column_count = column_count;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> DayPlannerConfig {
        DayPlannerConfig::default()
    }

    fn timed(start_min: u32, end_min: u32) -> ItemTiming {
        ItemTiming::Timed { start_min, end_min }
    }

    #[test]
    fn parses_range_and_start_only_and_unscheduled() {
        let plan = parse_day_plan(
            "- [ ] 08:00 – 11:00 Evaluate the plan\n\
             - [ ] 09:30 Standup\n\
             - [ ] Workout\n",
            &config(),
        );
        assert_eq!(
            plan.items,
            vec![
                PlanItem {
                    row: 0,
                    done: false,
                    weight: ItemWeight::Normal,
                    struck: false,
                    label: "Evaluate the plan".to_string(),
                    timing: timed(480, 660),
                    section: None,
                },
                PlanItem {
                    row: 1,
                    done: false,
                    weight: ItemWeight::Normal,
                    struck: false,
                    label: "Standup".to_string(),
                    timing: timed(570, 600),
                    section: None,
                },
                PlanItem {
                    row: 2,
                    done: false,
                    weight: ItemWeight::Normal,
                    struck: false,
                    label: "Workout".to_string(),
                    timing: ItemTiming::Unscheduled,
                    section: None,
                },
            ]
        );
    }

    #[test]
    fn parses_all_range_separators() {
        for line in [
            "- [ ] 8:00-11:00 T",
            "- [ ] 08:00 - 11:00 T",
            "- [ ] 08:00–11:00 T",
            "- [ ] 08:00 — 11:00 T",
            "- [ ] 08:00 to 11:00 T",
            "- [ ] 08:00to11:00 T",
        ] {
            let plan = parse_day_plan(line, &config());
            assert_eq!(plan.items.len(), 1, "for {line:?}");
            assert_eq!(plan.items[0].timing, timed(480, 660), "for {line:?}");
            assert_eq!(plan.items[0].label, "T", "for {line:?}");
        }
    }

    #[test]
    fn checkbox_state_and_bullet_markers() {
        let plan = parse_day_plan(
            "* [x] 09:00 Done task\n+ [X] Also done\n- [ ] Not done\n",
            &config(),
        );
        assert_eq!(
            plan.items.iter().map(|item| item.done).collect::<Vec<_>>(),
            vec![true, true, false]
        );
    }

    #[test]
    fn a_wholly_struck_task_is_unwrapped_and_marked_struck() {
        let plan = parse_day_plan(
            "- [ ] ~~09:00 – 10:00 Dropped meeting~~\n\
             - [ ] 09:00 ~~Dropped standup~~ <!--gcal:1-->\n\
             - [ ] ~~Dropped errand~~\n\
             - [ ] Call ~~Bob~~ Alice\n\
             - [ ] Plain task\n",
            &config(),
        );
        assert_eq!(
            plan.items
                .iter()
                .map(|item| (item.label.as_str(), item.struck, item.timing))
                .collect::<Vec<_>>(),
            vec![
                // A strikethrough around the time token still keeps the task
                // on the grid at its hour.
                ("Dropped meeting", true, timed(540, 600)),
                ("Dropped standup", true, timed(540, 570)),
                ("Dropped errand", true, ItemTiming::Unscheduled),
                ("Call ~~Bob~~ Alice", false, ItemTiming::Unscheduled),
                ("Plain task", false, ItemTiming::Unscheduled),
            ]
        );
    }

    #[test]
    fn non_candidates_are_ignored() {
        let plan = parse_day_plan(
            "# Heading\n\
             just a paragraph 09:00\n\
             - plain bullet 09:00\n\
             -[ ] no space after bullet\n\
             - [y] bad state\n\
             - [ ]no space after checkbox\n\
             1. [ ] numbered lists don't count\n",
            &config(),
        );
        assert_eq!(plan.items, vec![]);
    }

    #[test]
    fn nested_tasks_are_candidates() {
        let plan = parse_day_plan("  - [ ] 09:00 Nested\n\t* [x] Deep\n", &config());
        assert_eq!(plan.items.len(), 2);
        assert_eq!(plan.items[0].timing, timed(540, 570));
        assert_eq!(plan.items[0].row, 0);
        assert_eq!(plan.items[1].row, 1);
    }

    #[test]
    fn malformed_times_become_unscheduled_with_raw_label() {
        for text in [
            "25:99 Impossible",
            "8: Broken",
            "08:0 Short",
            "8:000 Long",
            "08:00x Glued",
            "123:00 Wide",
        ] {
            let plan = parse_day_plan(&format!("- [ ] {text}"), &config());
            assert_eq!(plan.items.len(), 1, "for {text:?}");
            assert_eq!(
                plan.items[0].timing,
                ItemTiming::Unscheduled,
                "for {text:?}"
            );
            assert_eq!(plan.items[0].label, text, "for {text:?}");
        }
    }

    #[test]
    fn malformed_range_end_falls_back_to_start_only() {
        let plan = parse_day_plan("- [ ] 08:00 - eat breakfast\n", &config());
        assert_eq!(plan.items[0].timing, timed(480, 510));
        assert_eq!(plan.items[0].label, "- eat breakfast");

        // A glued malformed range is not a time at all.
        let plan = parse_day_plan("- [ ] 08:00-11:00x tail\n", &config());
        assert_eq!(plan.items[0].timing, ItemTiming::Unscheduled);
        assert_eq!(plan.items[0].label, "08:00-11:00x tail");
    }

    #[test]
    fn range_end_at_or_before_start_gets_default_duration() {
        let plan = parse_day_plan(
            "- [ ] 11:00 – 08:00 Backwards\n- [ ] 09:00-09:00 Zero\n",
            &config(),
        );
        assert_eq!(plan.items[0].timing, timed(660, 690));
        assert_eq!(plan.items[1].timing, timed(540, 570));
    }

    #[test]
    fn end_clamps_to_end_of_day() {
        let plan = parse_day_plan("- [ ] 23:50 Late\n", &config());
        assert_eq!(plan.items[0].timing, timed(1430, 1440));
    }

    #[test]
    fn range_may_end_at_midnight() {
        let plan = parse_day_plan("- [ ] 23:00 – 24:00 Wind down\n", &config());
        assert_eq!(plan.items[0].timing, timed(1380, 1440));
        assert_eq!(plan.items[0].label, "Wind down");

        // But 24:00 is never a valid start.
        let plan = parse_day_plan("- [ ] 24:00 Impossible\n", &config());
        assert_eq!(plan.items[0].timing, ItemTiming::Unscheduled);
    }

    #[test]
    fn bare_checkbox_is_an_unscheduled_chip() {
        // Looser than the §5.1 grammar (which requires text after the
        // checkbox), deliberately: a just-typed `- [ ]` shows up as an
        // empty chip instead of vanishing.
        let plan = parse_day_plan("- [ ]\n- [ ] \n", &config());
        assert_eq!(plan.items.len(), 2);
        assert!(
            plan.items
                .iter()
                .all(|item| item.label.is_empty() && item.timing == ItemTiming::Unscheduled)
        );
    }

    #[test]
    fn duplicate_planner_headings_use_the_first() {
        let plan = parse_day_plan(
            "## Day planner\n- [ ] First\n## Break\n## Day planner\n- [ ] Second\n",
            &config(),
        );
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].label, "First");
    }

    #[test]
    fn empty_label_and_kept_metadata() {
        let plan = parse_day_plan("- [ ] 09:00\n- [ ] 10:00 Review #tag [[link]]\n", &config());
        assert_eq!(plan.items[0].timing, timed(540, 570));
        assert_eq!(plan.items[0].label, "");
        assert_eq!(plan.items[1].label, "Review #tag [[link]]");
    }

    #[test]
    fn heading_scopes_parsing() {
        let plan = parse_day_plan(
            "# Monday\n\
             - [ ] before the section\n\
             ## Day planner\n\
             - [ ] 09:00 In section\n\
             ### Subsection\n\
             - [ ] still in section\n\
             ## Personal\n\
             - [ ] after the section\n",
            &config(),
        );
        assert_eq!(
            plan.items
                .iter()
                .map(|item| item.label.as_str())
                .collect::<Vec<_>>(),
            vec!["In section", "still in section"]
        );
        assert_eq!(plan.items[0].row, 3);
    }

    #[test]
    fn heading_matches_case_insensitively() {
        let plan = parse_day_plan("## DAY PLANNER\n- [ ] 09:00 T\n", &config());
        assert_eq!(plan.items.len(), 1);
    }

    #[test]
    fn missing_heading_parses_whole_file() {
        let plan = parse_day_plan("# Monday\n- [ ] 09:00 One\n\n- [ ] Two\n", &config());
        assert_eq!(plan.items.len(), 2);
    }

    #[test]
    fn empty_heading_config_parses_whole_file() {
        let mut config = config();
        config.heading = String::new();
        let plan = parse_day_plan("## Day planner\n- [ ] One\n## Next\n- [ ] Two\n", &config);
        assert_eq!(plan.items.len(), 2);
    }

    #[test]
    fn sections_are_tracked_one_level_below_the_planner_heading() {
        let plan = parse_day_plan(
            "## Day planner\n\
             - [ ] Root item\n\
             ### Calendar\n\
             - [ ] 10:00 - 10:30 Meeting\n\
             #### Deeper\n\
             - [ ] Still calendar\n\
             ### Misc\n\
             - [ ] Errand\n\
             ## Personal\n\
             - [ ] Outside\n",
            &config(),
        );
        assert_eq!(
            plan.items
                .iter()
                .map(|item| (item.label.as_str(), item.section.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("Root item", None),
                ("Meeting", Some("Calendar")),
                ("Still calendar", Some("Calendar")),
                ("Errand", Some("Misc")),
            ]
        );
        assert_eq!(plan.sections(), vec!["Calendar", "Misc"]);
    }

    #[test]
    fn whole_file_parse_has_no_sections() {
        let mut config = config();
        config.heading = String::new();
        let plan = parse_day_plan("## Calendar\n- [ ] 09:00 T\n", &config);
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].section, None);
        assert!(plan.sections().is_empty());
    }

    #[test]
    fn trailing_html_comment_is_stripped_from_labels() {
        let plan = parse_day_plan(
            "- [x] 10:00 - 10:30 API Leads meeting <!--gcal:9f2c1ab4e7d0-->\n\
             - [ ] Own note <!-- remember this -->\n\
             - [ ] Not a comment --> here\n\
             - [ ] <!--only comment-->\n",
            &config(),
        );
        assert_eq!(plan.items[0].label, "API Leads meeting");
        assert_eq!(plan.items[1].label, "Own note");
        assert_eq!(plan.items[2].label, "Not a comment --> here");
        assert_eq!(plan.items[3].label, "");
    }

    #[test]
    fn strip_trailing_comment_ignores_mid_line_comments() {
        assert_eq!(strip_trailing_comment("a <!-- b --> c"), "a <!-- b --> c");
        assert_eq!(strip_trailing_comment("a <!-- b -->"), "a");
        assert_eq!(strip_trailing_comment("<!-->"), "<!-->");
        assert_eq!(strip_trailing_comment("plain"), "plain");
    }

    #[test]
    fn section_palette_index_is_stable_and_case_insensitive() {
        let index = section_palette_index("Meetings", 8);
        assert_eq!(section_palette_index("Meetings", 8), index);
        assert_eq!(section_palette_index("  meetings ", 8), index);
        assert!(index < 8);
        assert_eq!(section_palette_index("anything", 0), 0);
    }

    #[test]
    fn parse_grid_bound_accepts_end_of_day() {
        assert_eq!(parse_grid_bound("06:00"), Some(360));
        assert_eq!(parse_grid_bound("24:00"), Some(1440));
        assert_eq!(parse_grid_bound(" 9:30 "), Some(570));
        assert_eq!(parse_grid_bound("24:01"), None);
        assert_eq!(parse_grid_bound("6"), None);
        assert_eq!(parse_grid_bound("06:00pm"), None);
    }

    #[test]
    fn grid_bounds_expand_and_round_to_hours() {
        let config = config();
        let plan = parse_day_plan("- [ ] 05:30 – 06:30 Early\n- [ ] 23:45 Late\n", &config);
        assert_eq!(grid_bounds(&plan, &config), (300, 1440));

        let plan = parse_day_plan("- [ ] Unscheduled only\n", &config);
        assert_eq!(grid_bounds(&plan, &config), (360, 1440));
    }

    #[test]
    fn layout_non_overlapping_blocks_take_full_width() {
        let plan = parse_day_plan("- [ ] 08:00 – 09:00 A\n- [ ] 09:00 – 10:00 B\n", &config());
        let blocks = layout_blocks(&plan, 0);
        assert!(blocks.iter().all(|block| block.column_count == 1));
        assert!(blocks.iter().all(|block| block.column == 0));
    }

    #[test]
    fn layout_identical_times_get_two_columns() {
        let plan = parse_day_plan("- [ ] 09:00 – 10:00 A\n- [ ] 09:00 – 10:00 B\n", &config());
        let blocks = layout_blocks(&plan, 0);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].column, 0);
        assert_eq!(blocks[1].column, 1);
        assert!(blocks.iter().all(|block| block.column_count == 2));
    }

    #[test]
    fn layout_transitive_overlap_forms_one_cluster() {
        // A overlaps B, B overlaps C, but A doesn't overlap C: still one
        // cluster, and C reuses A's freed column.
        let plan = parse_day_plan(
            "- [ ] 08:00 – 09:00 A\n- [ ] 08:30 – 10:00 B\n- [ ] 09:00 – 10:00 C\n",
            &config(),
        );
        let blocks = layout_blocks(&plan, 0);
        assert_eq!(
            blocks
                .iter()
                .map(|block| (block.column, block.column_count))
                .collect::<Vec<_>>(),
            vec![(0, 2), (1, 2), (0, 2)]
        );
    }

    #[test]
    fn status_blocks_lay_out_in_their_own_lane() {
        // The focus block spans both meetings; without its own lane it would
        // halve the width of every one of them.
        let plan = parse_day_plan(
            "- [ ] 09:00 – 17:00 Focus time <!--gcal:aaaaaaaaaaaa:focus-->\n\
             - [ ] 09:00 – 10:00 Standup <!--gcal:bbbbbbbbbbbb-->\n\
             - [ ] 14:00 – 17:00 Implement licensing\n",
            &config(),
        );
        assert_eq!(
            plan.items
                .iter()
                .map(|item| item.weight)
                .collect::<Vec<_>>(),
            vec![ItemWeight::Status, ItemWeight::Normal, ItemWeight::Normal]
        );
        assert!(has_status_blocks(&plan));

        let blocks = layout_blocks(&plan, 0);
        assert_eq!(blocks.len(), 3);
        for block in &blocks {
            assert_eq!(
                (block.column, block.column_count),
                (0, 1),
                "item {} should own its lane's full width",
                block.item_index
            );
        }
    }

    #[test]
    fn overlapping_status_blocks_share_their_own_lane() {
        let plan = parse_day_plan(
            "- [ ] 09:00 – 17:00 Focus time <!--gcal:aaaaaaaaaaaa:focus-->\n\
             - [ ] 13:00 – 18:00 Out of office <!--gcal:bbbbbbbbbbbb:ooo-->\n\
             - [ ] 10:00 – 10:30 Standup\n",
            &config(),
        );
        let blocks = layout_blocks(&plan, 0);
        let column_counts: Vec<_> = blocks
            .iter()
            .map(|block| (block.item_index, block.column, block.column_count))
            .collect();
        // The two status blocks split the status lane; the meeting keeps the
        // whole main lane.
        assert!(column_counts.contains(&(2, 0, 1)), "{column_counts:?}");
        assert!(column_counts.contains(&(0, 0, 2)), "{column_counts:?}");
        assert!(column_counts.contains(&(1, 1, 2)), "{column_counts:?}");
    }

    #[test]
    fn a_day_without_status_blocks_reports_none() {
        let plan = parse_day_plan("- [ ] 09:00 – 10:00 Standup\n", &config());
        assert!(!has_status_blocks(&plan));
    }

    #[test]
    fn layout_separate_clusters_are_independent() {
        let plan = parse_day_plan(
            "- [ ] 08:00 – 09:00 A\n- [ ] 08:00 – 09:00 B\n- [ ] 14:00 – 15:00 C\n",
            &config(),
        );
        let blocks = layout_blocks(&plan, 0);
        assert_eq!(blocks[2].column_count, 1);
    }

    #[test]
    fn layout_short_tasks_overlap_by_visual_extent() {
        // 09:00–09:05 renders at the minimum block height, which covers
        // 09:10's slot: they must get separate columns.
        let plan = parse_day_plan(
            "- [ ] 09:00 – 09:05 Tiny\n- [ ] 09:10 – 09:40 Next\n",
            &config(),
        );
        let blocks = layout_blocks(&plan, 23);
        assert_eq!(blocks[0].column, 0);
        assert_eq!(blocks[1].column, 1);
        assert!(blocks.iter().all(|block| block.column_count == 2));

        // With no minimum, they don't overlap.
        let blocks = layout_blocks(&plan, 0);
        assert!(blocks.iter().all(|block| block.column_count == 1));
    }
}
