use eframe::egui::{
    self, vec2, Color32, CornerRadius, DragValue, Frame, Label, Margin, RichText, ScrollArea,
    Stroke, TextEdit,
};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const TALENTS_WIKI_URL: &str = "https://deepwoken.co/wiki/talent";
const TALENTS_CACHE_PATH: &str = "talents_wiki_cache.json";
const BASE_STATS: [&str; 6] = [
    "Strength",
    "Fortitude",
    "Agility",
    "Intelligence",
    "Willpower",
    "Charisma",
];
const WEAPON_STATS: [&str; 3] = ["Heavy Wep.", "Medium Wep.", "Light Wep."];
const ATTUNEMENT_STATS: [&str; 7] = [
    "Flamecharm",
    "Frostdraw",
    "Thundercall",
    "Galebreathe",
    "Shadowcast",
    "Ironsing",
    "Bloodrend",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TalentCategory {
    Pre,
    Post,
    Any,
}

impl TalentCategory {
    fn label(self) -> &'static str {
        match self {
            Self::Pre => "PRE",
            Self::Post => "POST",
            Self::Any => "ANY",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Eq, PartialEq)]
struct StatBlock {
    base: [u8; 6],
    weapon: [u8; 3],
    attunement: [u8; 7],
}

impl StatBlock {
    fn max_with(&mut self, other: &Self) {
        max_assign(&mut self.base, &other.base);
        max_assign(&mut self.weapon, &other.weapon);
        max_assign(&mut self.attunement, &other.attunement);
    }

    fn is_all_zero(&self) -> bool {
        self.base.iter().all(|value| *value == 0)
            && self.weapon.iter().all(|value| *value == 0)
            && self.attunement.iter().all(|value| *value == 0)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Talent {
    key: String,
    name: String,
    rarity: String,
    stats: StatBlock,
    #[serde(default)]
    requirement_notes: Vec<String>,
    #[serde(default)]
    variant_label: Option<String>,
}

impl Talent {
    fn display_name(&self) -> String {
        match &self.variant_label {
            Some(label) => format!("{} ({label})", self.name),
            None => self.name.clone(),
        }
    }

    fn search_blob(&self) -> String {
        let mut tokens = vec![
            self.name.to_lowercase(),
            self.key.to_lowercase(),
            self.rarity.to_lowercase(),
        ];
        if let Some(label) = &self.variant_label {
            tokens.push(label.to_lowercase());
        }

        append_requirement_search_tokens(&mut tokens, &BASE_STATS, &self.stats.base);
        append_requirement_search_tokens(&mut tokens, &WEAPON_STATS, &self.stats.weapon);
        append_requirement_search_tokens(&mut tokens, &ATTUNEMENT_STATS, &self.stats.attunement);
        tokens.extend(self.requirement_notes.iter().map(|line| line.to_lowercase()));

        tokens.join(" ")
    }

    fn requirement_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if !self.requirement_notes.is_empty() {
            lines.extend(self.requirement_notes.clone());
        } else {
            collect_stat_lines(&mut lines, &BASE_STATS, &self.stats.base);
            collect_stat_lines(&mut lines, &WEAPON_STATS, &self.stats.weapon);
            collect_stat_lines(&mut lines, &ATTUNEMENT_STATS, &self.stats.attunement);
        }
        if lines.is_empty() {
            lines.push("No stat requirements".to_owned());
        }
        lines
    }
}

#[derive(Clone, Debug)]
struct SelectedTalent {
    talent: Talent,
    category: TalentCategory,
}

#[derive(Clone, Debug)]
struct OwnedTalentPrereq {
    name: String,
    stats: StatBlock,
    category: TalentCategory,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ActiveTab {
    Talents,
    Stats,
    Output,
}

#[derive(Clone, Debug)]
struct OptimizationResult {
    points_spare: i32,
    output: String,
}

#[derive(Clone, Debug)]
struct BuildOptimizer {
    prerequisites: Vec<OwnedTalentPrereq>,
    best_result: Option<OptimizationResult>,
    error_message: Option<String>,
}

impl BuildOptimizer {
    fn new() -> Self {
        Self {
            prerequisites: Vec::new(),
            best_result: None,
            error_message: None,
        }
    }

    fn optimize_build(&mut self, prerequisites: Vec<OwnedTalentPrereq>) -> Result<OptimizationResult, String> {
        self.prerequisites = prerequisites;
        self.best_result = None;
        self.error_message = None;

        let fixed_pre: Vec<_> = self
            .prerequisites
            .iter()
            .filter(|prereq| prereq.category == TalentCategory::Pre)
            .cloned()
            .collect();
        let fixed_post: Vec<_> = self
            .prerequisites
            .iter()
            .filter(|prereq| prereq.category == TalentCategory::Post)
            .cloned()
            .collect();
        let any: Vec<_> = self
            .prerequisites
            .iter()
            .filter(|prereq| prereq.category == TalentCategory::Any)
            .cloned()
            .collect();

        let total_combinations = 1usize
            .checked_shl(any.len() as u32)
            .ok_or_else(|| "Too many ANY talents to evaluate.".to_owned())?;

        let mut best: Option<OptimizationResult> = None;

        for combo in 0..total_combinations {
            let mut current_pre = StatBlock::default();
            let mut current_post = StatBlock::default();

            for prereq in &fixed_pre {
                current_pre.max_with(&prereq.stats);
                apply_derived_post_requirements(&mut current_post, &prereq.stats);
            }

            for prereq in &fixed_post {
                current_post.max_with(&prereq.stats);
            }

            for (index, prereq) in any.iter().enumerate() {
                let treat_as_pre = ((combo >> index) & 1) == 0;
                if treat_as_pre {
                    current_pre.max_with(&prereq.stats);
                    apply_derived_post_requirements(&mut current_post, &prereq.stats);
                } else {
                    current_post.max_with(&prereq.stats);
                }
            }

            if impossible_combination(&current_pre, &current_post) {
                continue;
            }

            for pre_variant in zero_to_positive_variants(&current_pre, &current_post) {
                let Some((shrine_stats, shrine_points_spare)) = shrine_of_order(&pre_variant) else {
                    continue;
                };

                let points_needed = points_needed_for_post(&shrine_stats, &current_post);
                if points_needed > shrine_points_spare {
                    continue;
                }

                let final_stats = apply_post_requirements(&shrine_stats, &current_post);
                let points_spare = shrine_points_spare - points_needed;
                let result = OptimizationResult {
                    points_spare,
                    output: format_result(pre_variant, current_post, shrine_stats, final_stats, points_spare),
                };

                let replace = best
                    .as_ref()
                    .map(|current| result.points_spare > current.points_spare)
                    .unwrap_or(true);
                if replace {
                    best = Some(result);
                }
            }
        }

        if let Some(result) = best {
            self.best_result = Some(result.clone());
            Ok(result)
        } else {
            let error = "No valid build found.".to_owned();
            self.error_message = Some(error.clone());
            Err(error)
        }
    }
}

struct DeepwokenApp {
    talents: Vec<Talent>,
    selected_index: Option<usize>,
    selected_talents: Vec<SelectedTalent>,
    selected_selected_index: Option<usize>,
    search: String,
    status: String,
    active_tab: ActiveTab,
    pre_stats: StatBlock,
    post_stats: StatBlock,
    min_pre: StatBlock,
    min_post: StatBlock,
    optimizer: BuildOptimizer,
    output: String,
    theme_applied: bool,
}

impl DeepwokenApp {
    fn new() -> Self {
        match load_startup_talents() {
            Ok((talents, status)) => Self {
                talents,
                selected_index: None,
                selected_talents: Vec::new(),
                selected_selected_index: None,
                search: String::new(),
                status,
                active_tab: ActiveTab::Talents,
                pre_stats: StatBlock::default(),
                post_stats: StatBlock::default(),
                min_pre: StatBlock::default(),
                min_post: StatBlock::default(),
                optimizer: BuildOptimizer::new(),
                output: String::new(),
                theme_applied: false,
            },
            Err(error) => Self {
                talents: Vec::new(),
                selected_index: None,
                selected_talents: Vec::new(),
                selected_selected_index: None,
                search: String::new(),
                status: format!("Failed to load talents: {error}"),
                active_tab: ActiveTab::Talents,
                pre_stats: StatBlock::default(),
                post_stats: StatBlock::default(),
                min_pre: StatBlock::default(),
                min_post: StatBlock::default(),
                optimizer: BuildOptimizer::new(),
                output: String::new(),
                theme_applied: false,
            },
        }
    }

    fn filtered_talents(&self) -> Vec<usize> {
        let needle = self.search.trim().to_lowercase();
        self.talents
            .iter()
            .enumerate()
            .filter(|(_, talent)| needle.is_empty() || talent.search_blob().contains(&needle))
            .map(|(index, _)| index)
            .collect()
    }

    fn add_selected_talent(&mut self, category: TalentCategory) {
        let Some(index) = self.selected_index else {
            self.status = "Select a talent first.".to_owned();
            return;
        };

        let talent = self.talents[index].clone();
        self.selected_talents.push(SelectedTalent { talent, category });
        self.selected_selected_index = Some(self.selected_talents.len() - 1);
        self.recalculate_stat_minimums();
        self.status = format!("Added talent as {}.", category.label());
    }

    fn remove_selected_talent(&mut self) {
        let Some(index) = self.selected_selected_index else {
            self.status = "Select a chosen talent to remove.".to_owned();
            return;
        };

        self.selected_talents.remove(index);
        self.selected_selected_index = if self.selected_talents.is_empty() {
            None
        } else {
            Some(index.min(self.selected_talents.len() - 1))
        };
        self.recalculate_stat_minimums();
        self.status = "Removed selected talent.".to_owned();
    }

    fn clear_selected_talents(&mut self) {
        self.selected_talents.clear();
        self.selected_selected_index = None;
        self.recalculate_stat_minimums();
        self.output.clear();
        self.status = "Cleared selected talents.".to_owned();
    }

    fn recalculate_stat_minimums(&mut self) {
        self.min_pre = StatBlock::default();
        self.min_post = StatBlock::default();

        for selected in &self.selected_talents {
            match selected.category {
                TalentCategory::Pre => {
                    self.min_pre.max_with(&selected.talent.stats);
                }
                TalentCategory::Post => {
                    self.min_post.max_with(&selected.talent.stats);
                }
                TalentCategory::Any => {}
            }
        }

        apply_derived_post_requirements(&mut self.min_post, &self.min_pre);
        self.pre_stats = StatBlock::default();
        self.post_stats = StatBlock::default();
        clamp_to_minimums(&mut self.pre_stats, &self.min_pre);
        clamp_to_minimums(&mut self.post_stats, &self.min_post);
    }

    fn optimize(&mut self) {
        let mut prerequisites: Vec<OwnedTalentPrereq> = self
            .selected_talents
            .iter()
            .map(|selected| OwnedTalentPrereq {
                name: selected.talent.display_name(),
                stats: selected.talent.stats,
                category: selected.category,
            })
            .collect();

        if !self.pre_stats.is_all_zero() {
            prerequisites.push(OwnedTalentPrereq {
                name: "Manual PRE".to_owned(),
                stats: self.pre_stats,
                category: TalentCategory::Pre,
            });
        }

        if !self.post_stats.is_all_zero() {
            prerequisites.push(OwnedTalentPrereq {
                name: "Manual POST".to_owned(),
                stats: self.post_stats,
                category: TalentCategory::Post,
            });
        }

        let mut debug = String::from("=== DEBUG: Passing to optimizer ===\n");
        debug.push_str(&format!("Total prerequisites: {}\n", prerequisites.len()));
        for prereq in &prerequisites {
            debug.push_str(&format!("• {} ({})\n", prereq.name, prereq.category.label()));
            append_stat_block(&mut debug, &prereq.stats, "  ");
        }
        debug.push('\n');

        match self.optimizer.optimize_build(prerequisites) {
            Ok(result) => {
                self.output = format!("{debug}{}", result.output);
                self.status = format!("Optimization complete. {} points spare.", result.points_spare);
                self.active_tab = ActiveTab::Output;
            }
            Err(error) => {
                self.output = format!("{debug}ERROR: Build is not possible!\n{error}");
                self.status = "Build is not possible.".to_owned();
                self.active_tab = ActiveTab::Output;
            }
        }
    }

    fn tab_button(ui: &mut egui::Ui, active: bool, title: &str) -> bool {
        let mut button = egui::Button::new(RichText::new(title).size(15.0))
            .min_size(vec2(96.0, 38.0))
            .corner_radius(10.0);
        if active {
            button = button
                .fill(Color32::from_rgb(151, 88, 58))
                .stroke(Stroke::new(1.0, Color32::from_rgb(233, 185, 135)));
        } else {
            button = button
                .fill(Color32::from_rgb(35, 35, 38))
                .stroke(Stroke::new(1.0, Color32::from_rgb(66, 66, 72)));
        }
        ui.add(button).clicked()
    }

    fn render_talents_tab(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |columns| {
            columns[0].vertical(|ui| {
                section_frame().show(ui, |ui| {
                    section_header(ui, "Available Talents", "Search and inspect talent requirements.");
                    ui.add(
                        TextEdit::singleline(&mut self.search)
                            .hint_text("Search by name, key, or rarity")
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!("{} talents loaded", self.talents.len()))
                            .color(Color32::from_rgb(184, 184, 190)),
                    );

                    let filtered = self.filtered_talents();
                    ui.add_space(8.0);
                    ScrollArea::vertical()
                        .id_salt("available_talents")
                        .max_height(500.0)
                        .show(ui, |ui| {
                            for index in filtered {
                                let talent = &self.talents[index];
                                let selected = self.selected_index == Some(index);
                                talent_row(ui, selected, &talent.display_name(), &talent.rarity, || {
                                    self.selected_index = Some(index);
                                });
                            }
                        });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if action_button(ui, "Add as PRE", Color32::from_rgb(81, 117, 88)).clicked() {
                            self.add_selected_talent(TalentCategory::Pre);
                        }
                        if action_button(ui, "Add as POST", Color32::from_rgb(126, 84, 64)).clicked() {
                            self.add_selected_talent(TalentCategory::Post);
                        }
                        if action_button(ui, "Add as ANY", Color32::from_rgb(76, 89, 119)).clicked() {
                            self.add_selected_talent(TalentCategory::Any);
                        }
                    });

                    if let Some(index) = self.selected_index {
                        let talent = &self.talents[index];
                        ui.add_space(12.0);
                        inset_frame().show(ui, |ui| {
                            ui.label(RichText::new(talent.display_name()).size(20.0).strong());
                            ui.add_space(4.0);
                            ui.horizontal_wrapped(|ui| {
                                meta_pill(ui, format!("Key: {}", talent.key));
                                meta_pill(ui, format!("Rarity: {}", talent.rarity));
                            });
                            ui.add_space(8.0);
                            for line in talent.requirement_lines() {
                                ui.label(RichText::new(line).color(Color32::from_rgb(225, 225, 228)));
                            }
                        });
                    }
                });
            });

            columns[1].vertical(|ui| {
                section_frame().show(ui, |ui| {
                    section_header(ui, "Selected Talents", "These feed the optimizer and stat minimums.");
                    ui.label(
                        RichText::new(format!("{} selected", self.selected_talents.len()))
                            .color(Color32::from_rgb(184, 184, 190)),
                    );

                    ui.add_space(8.0);
                    ScrollArea::vertical()
                        .id_salt("selected_talents")
                        .max_height(500.0)
                        .show(ui, |ui| {
                            for (index, selected) in self.selected_talents.iter().enumerate() {
                                let is_selected = self.selected_selected_index == Some(index);
                                selected_talent_row(ui, is_selected, selected, || {
                                    self.selected_selected_index = Some(index);
                                });
                            }
                        });

                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if action_button(ui, "Remove", Color32::from_rgb(109, 68, 68)).clicked() {
                            self.remove_selected_talent();
                        }
                        if action_button(ui, "Clear All", Color32::from_rgb(91, 91, 98)).clicked() {
                            self.clear_selected_talents();
                        }
                    });

                    ui.add_space(12.0);
                    inset_frame().show(ui, |ui| {
                        ui.label(RichText::new("Category behavior").strong().size(16.0));
                        ui.add_space(6.0);
                        ui.label("PRE talents must be satisfied before Shrine of Order.");
                        ui.label("POST talents must be satisfied after Shrine of Order.");
                        ui.label("ANY talents are tested in both directions by the optimizer.");
                    });
                });
            });
        });
    }

    fn render_stats_tab(&mut self, ui: &mut egui::Ui) {
        ui.columns(2, |columns| {
            columns[0].vertical(|ui| {
                section_frame().show(ui, |ui| {
                    section_header(ui, "Pre-Shrine Stats", "Set values required before using Shrine of Order.");
                    ScrollArea::vertical()
                        .id_salt("pre_stats_scroll")
                        .max_height(ui.available_height() - 8.0)
                        .show(ui, |ui| {
                            draw_stat_editor(ui, &mut self.pre_stats, &self.min_pre, true);
                        });
                });
            });
            columns[1].vertical(|ui| {
                section_frame().show(ui, |ui| {
                    section_header(ui, "Post-Shrine Stats", "Set values you need after Shrine of Order.");
                    ScrollArea::vertical()
                        .id_salt("post_stats_scroll")
                        .max_height(ui.available_height() - 8.0)
                        .show(ui, |ui| {
                            draw_stat_editor(ui, &mut self.post_stats, &self.min_post, false);
                        });
                });
            });
        });
    }

    fn render_output_tab(&mut self, ui: &mut egui::Ui) {
        section_frame().show(ui, |ui| {
            section_header(ui, "Optimization Output", "Run the solver and inspect the resulting plan.");
            ScrollArea::vertical().show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(&mut self.output)
                        .desired_rows(36)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .interactive(false),
                );
            });
        });
    }
}

impl eframe::App for DeepwokenApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if !self.theme_applied {
            apply_theme(ctx);
            self.theme_applied = true;
        }

        egui::TopBottomPanel::top("top_panel").show(ctx, |ui| {
            header_frame().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("Deepwoken Build Optimizer")
                                .size(28.0)
                                .strong()
                                .color(Color32::from_rgb(244, 236, 220)),
                        );
                        ui.label(
                            RichText::new("Clickable talents, readable stat planning, Shrine-ready output.")
                                .color(Color32::from_rgb(183, 177, 168)),
                        );
                    });
                    ui.add_space(18.0);
                    status_badge(ui, &self.status);
                });
                ui.add_space(14.0);
                ui.horizontal(|ui| {
                    if Self::tab_button(ui, self.active_tab == ActiveTab::Talents, "Talents") {
                        self.active_tab = ActiveTab::Talents;
                    }
                    if Self::tab_button(ui, self.active_tab == ActiveTab::Stats, "Stats") {
                        self.active_tab = ActiveTab::Stats;
                    }
                    if Self::tab_button(ui, self.active_tab == ActiveTab::Output, "Output") {
                        self.active_tab = ActiveTab::Output;
                    }
                    ui.add_space(12.0);
                    if action_button(ui, "Calculate Build", Color32::from_rgb(151, 88, 58)).clicked() {
                        self.optimize();
                    }
                });
            });
        });

        egui::CentralPanel::default()
            .frame(Frame::NONE.inner_margin(Margin::same(18)))
            .show(ctx, |ui| match self.active_tab {
                ActiveTab::Talents => self.render_talents_tab(ui),
                ActiveTab::Stats => self.render_stats_tab(ui),
                ActiveTab::Output => self.render_output_tab(ui),
            });
    }
}

fn draw_stat_editor(ui: &mut egui::Ui, stats: &mut StatBlock, mins: &StatBlock, is_pre: bool) {
    let panel_id = if is_pre { "pre" } else { "post" };
    ui.label(
        RichText::new("Blue values are talent-driven minimums.")
            .color(Color32::from_rgb(133, 186, 220)),
    );
    ui.add_space(8.0);

    inset_frame().show(ui, |ui| {
        draw_stat_group(
            ui,
            &format!("{panel_id}_base_stats"),
            "Base Stats",
            &BASE_STATS,
            &mut stats.base,
            &mins.base,
            is_pre,
        );
    });
    ui.add_space(8.0);
    inset_frame().show(ui, |ui| {
        draw_stat_group(
            ui,
            &format!("{panel_id}_weapon_stats"),
            "Weapon Stats",
            &WEAPON_STATS,
            &mut stats.weapon,
            &mins.weapon,
            is_pre,
        );
    });
    ui.add_space(8.0);
    inset_frame().show(ui, |ui| {
        draw_stat_group(
            ui,
            &format!("{panel_id}_attunement_stats"),
            "Attunement Stats",
            &ATTUNEMENT_STATS,
            &mut stats.attunement,
            &mins.attunement,
            is_pre,
        );
    });
}

fn draw_stat_group<const N: usize>(
    ui: &mut egui::Ui,
    grid_id: &str,
    title: &str,
    labels: &[&str; N],
    values: &mut [u8; N],
    mins: &[u8; N],
    is_pre: bool,
) {
    ui.label(RichText::new(title).strong().size(17.0));
    ui.add_space(6.0);
    egui::Grid::new(grid_id)
        .num_columns(3)
        .spacing(vec2(14.0, 8.0))
        .show(ui, |ui| {
        for ((label, value), min) in labels.iter().zip(values.iter_mut()).zip(mins.iter()) {
            ui.label(RichText::new(*label).color(Color32::from_rgb(234, 232, 228)).size(14.0));
            let mut display = *value;
            ui.add(
                DragValue::new(&mut display)
                    .range(*min..=100)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .max_decimals(0),
            );
            *value = display.max(*min);

            if *min > 0 {
                let hint = if !is_pre { "from talents/shrine" } else { "from talents" };
                ui.label(
                    RichText::new(format!("min {min} ({hint})"))
                        .color(Color32::from_rgb(133, 186, 220)),
                );
            } else {
                ui.label(RichText::new("optional").color(Color32::from_rgb(128, 128, 134)));
            }
            ui.end_row();
        }
        });
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = vec2(10.0, 10.0);
    style.spacing.button_padding = vec2(14.0, 10.0);
    style.spacing.window_margin = Margin::same(14);
    style.visuals = egui::Visuals::dark();
    style.visuals.override_text_color = Some(Color32::from_rgb(232, 228, 221));
    style.visuals.panel_fill = Color32::from_rgb(20, 20, 22);
    style.visuals.extreme_bg_color = Color32::from_rgb(16, 16, 18);
    style.visuals.faint_bg_color = Color32::from_rgb(31, 31, 35);
    style.visuals.code_bg_color = Color32::from_rgb(15, 15, 17);
    style.visuals.window_fill = Color32::from_rgb(26, 26, 30);
    style.visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(26, 26, 30);
    style.visuals.widgets.inactive.bg_fill = Color32::from_rgb(37, 37, 42);
    style.visuals.widgets.hovered.bg_fill = Color32::from_rgb(56, 56, 63);
    style.visuals.widgets.active.bg_fill = Color32::from_rgb(88, 68, 57);
    style.visuals.widgets.open.bg_fill = Color32::from_rgb(46, 46, 52);
    style.visuals.selection.bg_fill = Color32::from_rgb(151, 88, 58);
    style.visuals.selection.stroke = Stroke::new(1.0, Color32::from_rgb(253, 223, 170));
    style.visuals.widgets.inactive.corner_radius = CornerRadius::same(10);
    style.visuals.widgets.hovered.corner_radius = CornerRadius::same(10);
    style.visuals.widgets.active.corner_radius = CornerRadius::same(10);
    style.visuals.widgets.noninteractive.corner_radius = CornerRadius::same(12);
    ctx.set_style(style);
}

fn header_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(27, 24, 22))
        .stroke(Stroke::new(1.0, Color32::from_rgb(73, 62, 55)))
        .corner_radius(16.0)
        .inner_margin(Margin::same(16))
}

fn section_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(24, 24, 27))
        .stroke(Stroke::new(1.0, Color32::from_rgb(52, 52, 58)))
        .corner_radius(16.0)
        .inner_margin(Margin::same(16))
}

fn inset_frame() -> Frame {
    Frame::new()
        .fill(Color32::from_rgb(19, 19, 22))
        .stroke(Stroke::new(1.0, Color32::from_rgb(47, 47, 52)))
        .corner_radius(12.0)
        .inner_margin(Margin::same(12))
}

fn section_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.label(
        RichText::new(title)
            .size(22.0)
            .strong()
            .color(Color32::from_rgb(243, 234, 221)),
    );
    ui.label(RichText::new(subtitle).color(Color32::from_rgb(174, 170, 162)));
    ui.add_space(8.0);
}

fn action_button<'a>(ui: &'a mut egui::Ui, label: &str, fill: Color32) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(label).strong())
            .min_size(vec2(110.0, 36.0))
            .fill(fill)
            .stroke(Stroke::new(1.0, fill.gamma_multiply(1.4)))
            .corner_radius(10.0),
    )
}

fn meta_pill(ui: &mut egui::Ui, text: String) {
    Frame::new()
        .fill(Color32::from_rgb(41, 35, 31))
        .stroke(Stroke::new(1.0, Color32::from_rgb(83, 70, 61)))
        .corner_radius(999.0)
        .inner_margin(Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(Color32::from_rgb(223, 208, 189)));
        });
}

fn status_badge(ui: &mut egui::Ui, text: &str) {
    Frame::new()
        .fill(Color32::from_rgb(33, 41, 47))
        .stroke(Stroke::new(1.0, Color32::from_rgb(60, 86, 101)))
        .corner_radius(999.0)
        .inner_margin(Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(Color32::from_rgb(182, 220, 240)));
        });
}

fn talent_row(ui: &mut egui::Ui, selected: bool, name: &str, rarity: &str, on_click: impl FnOnce()) {
    let response = ui.add(
        egui::Button::new("")
            .selected(selected)
            .fill(if selected {
                Color32::from_rgb(57, 43, 37)
            } else {
                Color32::from_rgb(31, 31, 34)
            })
            .stroke(Stroke::new(
                1.0,
                if selected {
                    Color32::from_rgb(182, 140, 110)
                } else {
                    Color32::from_rgb(49, 49, 54)
                },
            ))
            .corner_radius(10.0)
            .min_size(vec2(ui.available_width(), 42.0)),
    );
    if response.clicked() {
        on_click();
    }
    let rect = response.rect.shrink2(vec2(12.0, 8.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            let rarity_width = 92.0;
            let name_width = (ui.available_width() - rarity_width).max(60.0);
            ui.add_sized(
                [name_width, 22.0],
                Label::new(RichText::new(name).strong().size(15.0))
                    .truncate()
                    .selectable(false),
            );
            ui.add_space(8.0);
            ui.add_sized(
                [rarity_width - 8.0, 22.0],
                Label::new(
                    RichText::new(rarity)
                        .color(Color32::from_rgb(196, 171, 127))
                        .strong(),
                )
                .truncate()
                .selectable(false),
            );
        });
    });
}

fn selected_talent_row(
    ui: &mut egui::Ui,
    selected: bool,
    talent: &SelectedTalent,
    on_click: impl FnOnce(),
) {
    let tint = match talent.category {
        TalentCategory::Pre => Color32::from_rgb(80, 113, 86),
        TalentCategory::Post => Color32::from_rgb(124, 83, 65),
        TalentCategory::Any => Color32::from_rgb(79, 91, 120),
    };
    let response = ui.add(
        egui::Button::new("")
            .selected(selected)
            .fill(if selected {
                tint.gamma_multiply(0.42)
            } else {
                Color32::from_rgb(31, 31, 34)
            })
            .stroke(Stroke::new(1.0, tint))
            .corner_radius(10.0)
            .min_size(vec2(ui.available_width(), 48.0)),
    );
    if response.clicked() {
        on_click();
    }
    let rect = response.rect.shrink2(vec2(12.0, 8.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
        ui.horizontal(|ui| {
            let category_width = 52.0;
            let rarity_width = 92.0;
            let name_width = (ui.available_width() - category_width - rarity_width).max(60.0);
            ui.label(
                RichText::new(format!("[{}]", talent.category.label()))
                    .strong()
                    .color(tint),
            );
            ui.add_sized(
                [name_width, 22.0],
                Label::new(RichText::new(talent.talent.display_name()).strong())
                    .truncate()
                    .selectable(false),
            );
            ui.add_space(8.0);
            ui.add_sized(
                [rarity_width, 22.0],
                Label::new(
                    RichText::new(&talent.talent.rarity)
                        .color(Color32::from_rgb(196, 171, 127))
                        .strong(),
                )
                .truncate()
                .selectable(false),
            );
        });
    });
}

fn collect_stat_lines<const N: usize>(lines: &mut Vec<String>, names: &[&str; N], values: &[u8; N]) {
    for (name, value) in names.iter().zip(values.iter()) {
        if *value > 0 {
            lines.push(format!("{name}: {value}"));
        }
    }
}

fn append_requirement_search_tokens<const N: usize>(
    tokens: &mut Vec<String>,
    names: &[&str; N],
    values: &[u8; N],
) {
    for (name, value) in names.iter().zip(values.iter()) {
        if *value > 0 {
            tokens.push(name.to_lowercase());
            tokens.push(format!("{} {}", name.to_lowercase(), value));
        }
    }
}

fn assign_chip_requirement(stats: &mut StatBlock, label: &str, value: u8) {
    match label {
        "Strength" => stats.base[0] = stats.base[0].max(value),
        "Fortitude" => stats.base[1] = stats.base[1].max(value),
        "Agility" => stats.base[2] = stats.base[2].max(value),
        "Intelligence" => stats.base[3] = stats.base[3].max(value),
        "Willpower" => stats.base[4] = stats.base[4].max(value),
        "Charisma" => stats.base[5] = stats.base[5].max(value),
        "Weapon" | "Mind" | "Body" | "Attunement" => {}
        "Heavy Weapon" | "Heavy Wep." => stats.weapon[0] = stats.weapon[0].max(value),
        "Medium Weapon" | "Medium Wep." => stats.weapon[1] = stats.weapon[1].max(value),
        "Light Weapon" | "Light Wep." => stats.weapon[2] = stats.weapon[2].max(value),
        "Flamecharm" => stats.attunement[0] = stats.attunement[0].max(value),
        "Frostdraw" => stats.attunement[1] = stats.attunement[1].max(value),
        "Thundercall" => stats.attunement[2] = stats.attunement[2].max(value),
        "Galebreathe" => stats.attunement[3] = stats.attunement[3].max(value),
        "Shadowcast" => stats.attunement[4] = stats.attunement[4].max(value),
        "Ironsing" => stats.attunement[5] = stats.attunement[5].max(value),
        "Bloodrend" | "Bloodrender" => stats.attunement[6] = stats.attunement[6].max(value),
        _ => {}
    }
}

fn load_startup_talents() -> Result<(Vec<Talent>, String), String> {
    match refresh_talents_cache() {
        Ok(()) => {
            let contents = fs::read_to_string(TALENTS_CACHE_PATH).map_err(|error| error.to_string())?;
            let talents = parse_talent_cache(&contents)?;
            Ok((talents, "Refreshed talents from wiki.".to_owned()))
        }
        Err(fetch_error) => {
            if Path::new(TALENTS_CACHE_PATH).exists() {
                let contents =
                    fs::read_to_string(TALENTS_CACHE_PATH).map_err(|error| error.to_string())?;
                let talents = parse_talent_cache(&contents)?;
                Ok((talents, format!("Using cached wiki talents. Refresh failed: {fetch_error}")))
            } else {
                Err(format!("Failed to load talents from wiki and no cache exists: {fetch_error}"))
            }
        }
    }
}

fn refresh_talents_cache() -> Result<(), String> {
    if Path::new(TALENTS_CACHE_PATH).exists() {
        fs::remove_file(TALENTS_CACHE_PATH).map_err(|error| error.to_string())?;
    }

    let client = Client::builder()
        .user_agent(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36",
        )
        .build()
        .map_err(|error| error.to_string())?;

    let talents = fetch_all_wiki_talents(&client)?;
    let cache = serde_json::to_string(&talents).map_err(|error| error.to_string())?;
    fs::write(TALENTS_CACHE_PATH, cache).map_err(|error| error.to_string())
}

fn fetch_all_wiki_talents(client: &Client) -> Result<Vec<Talent>, String> {
    let response = client
        .get(TALENTS_WIKI_URL)
        .header("Referer", "https://deepwoken.co/talents")
        .send()
        .and_then(|response| response.error_for_status())
        .map_err(|error| error.to_string())?;
    let body = response.text().map_err(|error| error.to_string())?;
    parse_talents_from_wiki_html(&body)
}

fn parse_talent_cache(json: &str) -> Result<Vec<Talent>, String> {
    let talents: Vec<Talent> = serde_json::from_str(json).map_err(|error| error.to_string())?;
    Ok(dedup_and_sort_talents(talents))
}

fn parse_talents_from_wiki_html(html: &str) -> Result<Vec<Talent>, String> {
    let payload = extract_nuxt_payload(html)?;
    let root: Vec<Value> = serde_json::from_str(payload).map_err(|error| error.to_string())?;
    let state = root
        .get(3)
        .and_then(Value::as_object)
        .ok_or_else(|| "Nuxt payload missing talent state object.".to_owned())?;
    let list_ref = state
        .get("wiki-list-talent")
        .and_then(nuxt_ref_index)
        .ok_or_else(|| "Nuxt payload missing wiki-list-talent reference.".to_owned())?;
    let list = root
        .get(list_ref)
        .and_then(Value::as_array)
        .ok_or_else(|| "Nuxt talent list reference did not resolve to an array.".to_owned())?;

    let mut talents = Vec::new();
    for entry_ref in list {
        let Some(entry_index) = nuxt_ref_index(entry_ref) else {
            continue;
        };
        let Some(entry) = root.get(entry_index).and_then(Value::as_object) else {
            continue;
        };

        let Some(name) = entry.get("name").and_then(|value| nuxt_string(&root, value)) else {
            continue;
        };
        let rarity = entry
            .get("rarity")
            .and_then(|value| nuxt_string(&root, value))
            .unwrap_or_else(|| "Common".to_owned());

        if entry.get("VOI").and_then(|value| nuxt_bool(&root, value)).unwrap_or(false) {
            continue;
        }

        let key = format!("{}::{}", name.to_lowercase(), entry_index);

        if let Some(requirements) = entry.get("requirements").and_then(|value| nuxt_object(&root, value)) {
            let variants =
                expand_requirement_variants(&root, requirements, RequirementVariant::default(), false);
            for variant in variants {
                let variant_label = if variant.variant_labels.is_empty() {
                    None
                } else {
                    Some(variant.variant_labels.join(" / "))
                };
                let variant_key = match &variant_label {
                    Some(label) => format!("{key}::{label}").to_lowercase(),
                    None => key.clone(),
                };

                talents.push(Talent {
                    key: variant_key,
                    name: name.clone(),
                    rarity: rarity.clone(),
                    stats: variant.stats,
                    requirement_notes: dedup_strings(variant.requirement_notes),
                    variant_label,
                });
            }
        } else {
            talents.push(Talent {
                key,
                name,
                rarity,
                stats: StatBlock::default(),
                requirement_notes: Vec::new(),
                variant_label: None,
            });
        }
    }

    Ok(dedup_and_sort_talents(talents))
}

fn extract_nuxt_payload(html: &str) -> Result<&str, String> {
    let start_marker = "<script type=\"application/json\" data-nuxt-data=\"nuxt-app\" data-ssr=\"true\" id=\"__NUXT_DATA__\">";
    let start = html
        .find(start_marker)
        .ok_or_else(|| "Could not find __NUXT_DATA__ script.".to_owned())?;
    let content_start = start + start_marker.len();
    let end = html[content_start..]
        .find("</script>")
        .map(|offset| content_start + offset)
        .ok_or_else(|| "Could not find end of __NUXT_DATA__ script.".to_owned())?;
    Ok(&html[content_start..end])
}

fn nuxt_ref_index(value: &Value) -> Option<usize> {
    value.as_u64().map(|value| value as usize)
}

fn nuxt_deref<'a>(root: &'a [Value], value: &'a Value) -> Option<&'a Value> {
    let index = nuxt_ref_index(value)?;
    root.get(index)
}

fn nuxt_string(root: &[Value], value: &Value) -> Option<String> {
    if let Some(text) = value.as_str() {
        return Some(text.to_owned());
    }
    nuxt_deref(root, value)?.as_str().map(str::to_owned)
}

fn nuxt_object<'a>(root: &'a [Value], value: &'a Value) -> Option<&'a Map<String, Value>> {
    if let Some(object) = value.as_object() {
        return Some(object);
    }
    nuxt_deref(root, value)?.as_object()
}

fn nuxt_array<'a>(root: &'a [Value], value: &'a Value) -> Option<&'a Vec<Value>> {
    if let Some(array) = value.as_array() {
        return Some(array);
    }
    nuxt_deref(root, value)?.as_array()
}

fn nuxt_u8(root: &[Value], value: &Value) -> Option<u8> {
    if let Some(number) = value.as_u64() {
        if let Some(resolved) = root.get(number as usize) {
            return resolved
                .as_u64()
                .and_then(|resolved_number| u8::try_from(resolved_number).ok())
                .or_else(|| u8::try_from(number).ok());
        }
        return u8::try_from(number).ok();
    }
    None
}

fn nuxt_bool(root: &[Value], value: &Value) -> Option<bool> {
    if let Some(boolean) = value.as_bool() {
        return Some(boolean);
    }
    nuxt_deref(root, value)?.as_bool()
}

#[derive(Clone, Debug, Default)]
struct RequirementVariant {
    stats: StatBlock,
    requirement_notes: Vec<String>,
    variant_labels: Vec<String>,
}

fn expand_requirement_variants(
    root: &[Value],
    requirements: &Map<String, Value>,
    base: RequirementVariant,
    nested_or: bool,
) -> Vec<RequirementVariant> {
    let mut variants = vec![base];

    if let Some(stat_map) = requirements.get("stats").and_then(|value| nuxt_object(root, value)) {
        for (label, raw_value) in stat_map {
            if let Some(value) = nuxt_u8(root, raw_value) {
                if label == "Weapon" {
                    let mut expanded = Vec::new();
                    for variant in variants {
                        for (weapon_label, weapon_index) in
                            [("Heavy Weapon", 0usize), ("Medium Weapon", 1usize), ("Light Weapon", 2usize)]
                        {
                            let mut next = variant.clone();
                            next.stats.weapon[weapon_index] = next.stats.weapon[weapon_index].max(value);
                            push_requirement_note(
                                &mut next.requirement_notes,
                                nested_or,
                                format!("{weapon_label}: {value}"),
                            );
                            next.variant_labels.push(weapon_label.to_owned());
                            expanded.push(next);
                        }
                    }
                    variants = expanded;
                } else if label == "Mind" {
                    let mut expanded = Vec::new();
                    for variant in variants {
                        for (mind_label, base_index) in
                            [("Intelligence", 3usize), ("Willpower", 4usize), ("Charisma", 5usize)]
                        {
                            let mut next = variant.clone();
                            next.stats.base[base_index] = next.stats.base[base_index].max(value);
                            push_requirement_note(
                                &mut next.requirement_notes,
                                nested_or,
                                format!("{mind_label}: {value}"),
                            );
                            next.variant_labels.push(mind_label.to_owned());
                            expanded.push(next);
                        }
                    }
                    variants = expanded;
                } else if label == "Body" {
                    let mut expanded = Vec::new();
                    for variant in variants {
                        for (body_label, base_index) in
                            [("Strength", 0usize), ("Fortitude", 1usize), ("Agility", 2usize)]
                        {
                            let mut next = variant.clone();
                            next.stats.base[base_index] = next.stats.base[base_index].max(value);
                            push_requirement_note(
                                &mut next.requirement_notes,
                                nested_or,
                                format!("{body_label}: {value}"),
                            );
                            next.variant_labels.push(body_label.to_owned());
                            expanded.push(next);
                        }
                    }
                    variants = expanded;
                } else if label == "Attunement" {
                    let mut expanded = Vec::new();
                    for variant in variants {
                        for (att_label, att_index) in [
                            ("Flamecharm", 0usize),
                            ("Frostdraw", 1usize),
                            ("Thundercall", 2usize),
                            ("Galebreathe", 3usize),
                            ("Shadowcast", 4usize),
                            ("Ironsing", 5usize),
                            ("Bloodrend", 6usize),
                        ] {
                            let mut next = variant.clone();
                            next.stats.attunement[att_index] = next.stats.attunement[att_index].max(value);
                            push_requirement_note(
                                &mut next.requirement_notes,
                                nested_or,
                                format!("{att_label}: {value}"),
                            );
                            next.variant_labels.push(att_label.to_owned());
                            expanded.push(next);
                        }
                    }
                    variants = expanded;
                } else {
                    for variant in &mut variants {
                        assign_chip_requirement(&mut variant.stats, label, value);
                        push_requirement_note(
                            &mut variant.requirement_notes,
                            nested_or,
                            format!("{label}: {value}"),
                        );
                    }
                }
            }
        }
    }

    for key in ["weapon", "equipment", "outfit", "origin", "memento"] {
        if let Some(text) = requirements.get(key).and_then(|value| nuxt_string(root, value)) {
            for variant in &mut variants {
                push_requirement_note(
                    &mut variant.requirement_notes,
                    nested_or,
                    format!("{}: {}", title_case(key), text),
                );
            }
        }
    }

    for key in ["talents", "mantras", "objectives", "quests"] {
        if let Some(items) = requirements.get(key).and_then(|value| nuxt_array(root, value)) {
            let mut resolved = Vec::new();
            for item in items {
                if let Some(text) = nuxt_string(root, item) {
                    resolved.push(text);
                }
            }
            if !resolved.is_empty() {
                for variant in &mut variants {
                    push_requirement_note(
                        &mut variant.requirement_notes,
                        nested_or,
                        format!("{}: {}", title_case(key), resolved.join(", ")),
                    );
                }
            }
        }
    }

    if let Some(or_items) = requirements.get("or").and_then(|value| nuxt_array(root, value)) {
        let current = std::mem::take(&mut variants);
        let mut expanded = Vec::new();
        for variant in current {
            for option in or_items {
                if let Some(option_requirements) = nuxt_object(root, option) {
                    expanded.extend(expand_requirement_variants(root, option_requirements, variant.clone(), true));
                } else if let Some(text) = nuxt_string(root, option) {
                    let mut next = variant.clone();
                    push_requirement_note(&mut next.requirement_notes, true, text.clone());
                    next.variant_labels.push(text);
                    expanded.push(next);
                }
            }
        }
        if !expanded.is_empty() {
            return expanded;
        }
    }

    variants
}

fn push_requirement_note(notes: &mut Vec<String>, nested_or: bool, text: String) {
    if nested_or {
        notes.push(format!("OR {text}"));
    } else {
        notes.push(text);
    }
}

fn title_case(key: &str) -> String {
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn dedup_strings(items: Vec<String>) -> Vec<String> {
    let mut deduped = Vec::new();
    for item in items {
        if !deduped.contains(&item) {
            deduped.push(item);
        }
    }
    deduped
}

fn dedup_and_sort_talents(talents: Vec<Talent>) -> Vec<Talent> {
    let mut by_key = BTreeMap::new();
    for talent in talents {
        by_key.insert(talent.key.clone(), talent);
    }

    let mut deduped: Vec<_> = by_key.into_values().collect();
    deduped.sort_by(|left, right| left.name.cmp(&right.name));
    deduped
}

fn max_assign<const N: usize>(target: &mut [u8; N], source: &[u8; N]) {
    for (left, right) in target.iter_mut().zip(source.iter()) {
        *left = (*left).max(*right);
    }
}

fn apply_derived_post_requirements(post: &mut StatBlock, pre: &StatBlock) {
    for (target, source) in post.base.iter_mut().zip(pre.base.iter()) {
        *target = (*target).max(source.saturating_sub(25));
    }
    for (target, source) in post.weapon.iter_mut().zip(pre.weapon.iter()) {
        *target = (*target).max(source.saturating_sub(25));
    }
    for (target, source) in post.attunement.iter_mut().zip(pre.attunement.iter()) {
        if *source > 0 {
            *target = (*target).max(1);
        }
    }
}

fn clamp_to_minimums(current: &mut StatBlock, mins: &StatBlock) {
    for (value, min) in current.base.iter_mut().zip(mins.base.iter()) {
        *value = (*value).max(*min);
    }
    for (value, min) in current.weapon.iter_mut().zip(mins.weapon.iter()) {
        *value = (*value).max(*min);
    }
    for (value, min) in current.attunement.iter_mut().zip(mins.attunement.iter()) {
        *value = (*value).max(*min);
    }
}

fn impossible_combination(pre: &StatBlock, post: &StatBlock) -> bool {
    for (pre_value, post_value) in pre.base.iter().zip(post.base.iter()) {
        if *post_value > 0 && *pre_value > *post_value && (*pre_value - *post_value) > 25 {
            return true;
        }
    }
    for (pre_value, post_value) in pre.weapon.iter().zip(post.weapon.iter()) {
        if *post_value > 0 && *pre_value > *post_value && (*pre_value - *post_value) > 25 {
            return true;
        }
    }
    false
}

#[derive(Clone, Copy)]
enum ZeroSlot {
    Base(usize),
    Weapon(usize),
    Attunement(usize),
}

fn zero_to_positive_variants(pre: &StatBlock, post: &StatBlock) -> Vec<StatBlock> {
    let mut slots = Vec::new();
    for (index, (pre_value, post_value)) in pre.base.iter().zip(post.base.iter()).enumerate() {
        if *pre_value == 0 && *post_value > 0 {
            slots.push(ZeroSlot::Base(index));
        }
    }
    for (index, (pre_value, post_value)) in pre.weapon.iter().zip(post.weapon.iter()).enumerate() {
        if *pre_value == 0 && *post_value > 0 {
            slots.push(ZeroSlot::Weapon(index));
        }
    }
    for (index, (pre_value, post_value)) in pre.attunement.iter().zip(post.attunement.iter()).enumerate() {
        if *pre_value == 0 && *post_value > 0 {
            slots.push(ZeroSlot::Attunement(index));
        }
    }

    let mut variants = vec![*pre];
    for slot in slots {
        let post_value = read_zero_slot(post, slot);
        let candidates = candidate_pre_values(post_value);
        let mut next_variants = Vec::new();
        for variant in variants {
            for candidate in &candidates {
                let mut next = variant;
                write_zero_slot(&mut next, slot, *candidate);
                if !next_variants.contains(&next) {
                    next_variants.push(next);
                }
            }
        }
        variants = next_variants;
    }

    variants
}

fn candidate_pre_values(post_value: u8) -> Vec<u8> {
    let mut candidates = vec![0];
    if post_value > 0 && !candidates.contains(&1) {
        candidates.push(1);
    }
    if !candidates.contains(&post_value) {
        candidates.push(post_value);
    }
    candidates
}

fn read_zero_slot(stats: &StatBlock, slot: ZeroSlot) -> u8 {
    match slot {
        ZeroSlot::Base(index) => stats.base[index],
        ZeroSlot::Weapon(index) => stats.weapon[index],
        ZeroSlot::Attunement(index) => stats.attunement[index],
    }
}

fn write_zero_slot(stats: &mut StatBlock, slot: ZeroSlot, value: u8) {
    match slot {
        ZeroSlot::Base(index) => stats.base[index] = value,
        ZeroSlot::Weapon(index) => stats.weapon[index] = value,
        ZeroSlot::Attunement(index) => stats.attunement[index] = value,
    }
}

fn shrine_of_order(pre: &StatBlock) -> Option<(StatBlock, i32)> {
    let mut shrine = *pre;
    let mut total_to_distribute = 0i32;
    let mut post_stats: Vec<(StatSection, usize)> = Vec::new();
    let mut non_zero_attunements = 0i32;

    for index in 0..6 {
        let value = &mut shrine.base[index];
        if *value >= 1 {
            if *value >= 26 {
                *value -= 25;
                total_to_distribute += 25;
            } else {
                total_to_distribute += (*value as i32) - 1;
                *value = 1;
            }
            post_stats.push((StatSection::Base, index));
        }
    }

    for index in 0..3 {
        let value = &mut shrine.weapon[index];
        if *value >= 1 {
            if *value >= 26 {
                *value -= 25;
                total_to_distribute += 25;
            } else {
                total_to_distribute += (*value as i32) - 1;
                *value = 1;
            }
            post_stats.push((StatSection::Weapon, index));
        }
    }

    for index in 0..7 {
        let value = &mut shrine.attunement[index];
        if *value >= 1 {
            total_to_distribute += (*value as i32) - 1;
            *value = 1;
            non_zero_attunements += 1;
            post_stats.push((StatSection::Attunement, index));
        }
    }

    if post_stats.is_empty() {
        return Some((shrine, 330));
    }

    while total_to_distribute > 0 {
        let mut current_values: Vec<u8> = post_stats
            .iter()
            .map(|(section, index)| read_stat(&shrine, *section, *index))
            .collect();
        current_values.sort_unstable();

        let lowest = current_values[0];
        let second_lowest = current_values
            .iter()
            .copied()
            .find(|value| *value > lowest)
            .unwrap_or(lowest);

        if lowest == second_lowest {
            let count = post_stats.len() as i32;
            if total_to_distribute >= count {
                let add_each = total_to_distribute / count;
                let remainder = total_to_distribute % count;
                for (offset, (section, index)) in post_stats.iter().enumerate() {
                    let mut new_value = read_stat(&shrine, *section, *index) as i32 + add_each;
                    if remainder > 0 && offset == 0 {
                        new_value += remainder;
                    }
                    write_stat(&mut shrine, *section, *index, new_value as u8);
                }
            }
            break;
        }

        let lowest_count = post_stats
            .iter()
            .filter(|(section, index)| read_stat(&shrine, *section, *index) == lowest)
            .count() as i32;
        let difference = (second_lowest - lowest) as i32;

        if total_to_distribute < lowest_count {
            break;
        }

        if difference > 0 && total_to_distribute >= difference * lowest_count {
            for (section, index) in &post_stats {
                if read_stat(&shrine, *section, *index) == lowest {
                    let new_value = read_stat(&shrine, *section, *index) as i32 + difference;
                    write_stat(&mut shrine, *section, *index, new_value as u8);
                    total_to_distribute -= difference;
                }
            }
        } else {
            let add_each = total_to_distribute / lowest_count;
            if add_each == 0 {
                break;
            }
            for (section, index) in &post_stats {
                if read_stat(&shrine, *section, *index) == lowest {
                    let new_value = read_stat(&shrine, *section, *index) as i32 + add_each;
                    write_stat(&mut shrine, *section, *index, new_value as u8);
                }
            }
            break;
        }
    }

    let points_spent = shrine.base.iter().map(|value| *value as i32).sum::<i32>()
        + shrine.weapon.iter().map(|value| *value as i32).sum::<i32>()
        + shrine.attunement.iter().map(|value| *value as i32).sum::<i32>();
    let mut total_points_spare = 330 - points_spent;
    if non_zero_attunements > 1 {
        total_points_spare += non_zero_attunements - 1;
    }

    Some((shrine, total_points_spare))
}

fn points_needed_for_post(shrine: &StatBlock, post: &StatBlock) -> i32 {
    deficit_sum(&shrine.base, &post.base)
        + deficit_sum(&shrine.weapon, &post.weapon)
        + deficit_sum(&shrine.attunement, &post.attunement)
}

fn deficit_sum<const N: usize>(current: &[u8; N], required: &[u8; N]) -> i32 {
    current
        .iter()
        .zip(required.iter())
        .map(|(current, required)| (*required as i32 - *current as i32).max(0))
        .sum()
}

fn apply_post_requirements(shrine: &StatBlock, post: &StatBlock) -> StatBlock {
    let mut final_stats = *shrine;
    max_assign(&mut final_stats.base, &post.base);
    max_assign(&mut final_stats.weapon, &post.weapon);
    max_assign(&mut final_stats.attunement, &post.attunement);
    final_stats
}

fn append_stat_block(output: &mut String, stats: &StatBlock, prefix: &str) {
    append_named_stats(output, prefix, &BASE_STATS, &stats.base);
    append_named_stats(output, prefix, &WEAPON_STATS, &stats.weapon);
    append_named_stats(output, prefix, &ATTUNEMENT_STATS, &stats.attunement);
}

fn append_named_stats<const N: usize>(
    output: &mut String,
    prefix: &str,
    labels: &[&str; N],
    values: &[u8; N],
) {
    for (label, value) in labels.iter().zip(values.iter()) {
        if *value > 0 {
            output.push_str(&format!("{prefix}{label}: {value}\n"));
        }
    }
}

fn format_result(
    pre: StatBlock,
    post: StatBlock,
    shrine: StatBlock,
    final_stats: StatBlock,
    points_spare: i32,
) -> String {
    let mut output = String::new();
    output.push_str("=== BUILD OPTIMIZATION RESULTS ===\n\n");
    output.push_str("=========================================\n");
    output.push_str("optimal build found\n");
    output.push_str("=========================================\n");
    output.push_str(&format!("points spare: {points_spare}\n\n"));

    output.push_str("=========================================\n");
    output.push_str("pre shrine stats\n");
    output.push_str("=========================================\n");
    append_stat_block(&mut output, &pre, "");
    output.push('\n');

    output.push_str("=========================================\n");
    output.push_str("post shrine stats - before adding points\n");
    output.push_str("=========================================\n");
    append_diff_block(&mut output, &pre, &shrine);
    output.push('\n');

    output.push_str("=========================================\n");
    output.push_str("post shrine stats - after adding points\n");
    output.push_str("=========================================\n");
    append_added_block(&mut output, &shrine, &final_stats, &post);

    output
}

fn append_diff_block(output: &mut String, pre: &StatBlock, shrine: &StatBlock) {
    append_changed_stats(output, &BASE_STATS, &pre.base, &shrine.base, &[0; 6]);
    append_changed_stats(output, &WEAPON_STATS, &pre.weapon, &shrine.weapon, &[0; 3]);
    append_changed_stats(
        output,
        &ATTUNEMENT_STATS,
        &pre.attunement,
        &shrine.attunement,
        &[0; 7],
    );
}

fn append_added_block(output: &mut String, shrine: &StatBlock, final_stats: &StatBlock, post: &StatBlock) {
    append_changed_stats(output, &BASE_STATS, &shrine.base, &final_stats.base, &post.base);
    append_changed_stats(output, &WEAPON_STATS, &shrine.weapon, &final_stats.weapon, &post.weapon);
    append_changed_stats(
        output,
        &ATTUNEMENT_STATS,
        &shrine.attunement,
        &final_stats.attunement,
        &post.attunement,
    );
}

fn append_changed_stats<const N: usize>(
    output: &mut String,
    labels: &[&str; N],
    before: &[u8; N],
    after: &[u8; N],
    required: &[u8; N],
) {
    for (((label, before), after), required) in labels
        .iter()
        .zip(before.iter())
        .zip(after.iter())
        .zip(required.iter())
    {
        if *after > 0 || *required > 0 {
            let delta = *after as i32 - *before as i32;
            output.push_str(&format!("{label}: {after} "));
            if delta >= 0 {
                output.push_str(&format!("(+{delta})\n"));
            } else {
                output.push_str(&format!("({delta})\n"));
            }
        }
    }
}

#[derive(Clone, Copy)]
enum StatSection {
    Base,
    Weapon,
    Attunement,
}

fn read_stat(stats: &StatBlock, section: StatSection, index: usize) -> u8 {
    match section {
        StatSection::Base => stats.base[index],
        StatSection::Weapon => stats.weapon[index],
        StatSection::Attunement => stats.attunement[index],
    }
}

fn write_stat(stats: &mut StatBlock, section: StatSection, index: usize, value: u8) {
    match section {
        StatSection::Base => stats.base[index] = value,
        StatSection::Weapon => stats.weapon[index] = value,
        StatSection::Attunement => stats.attunement[index] = value,
    }
}

fn main() -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Deepwoken Build Optimizer")
            .with_inner_size([1440.0, 960.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Deepwoken Build Optimizer",
        options,
        Box::new(|_cc| Ok(Box::new(DeepwokenApp::new()))),
    )
}
