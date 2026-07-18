//! Generic schema-driven editor for provider-neutral trigger programs.

use std::collections::BTreeMap;

use signal_processing::{
    CaptureChannelId, RegisteredTriggerPredicateSchema, SimpleTriggerCondition, TriggerCount,
    TriggerCountMode, TriggerEditorSchema, TriggerIdentifier, TriggerLogicOperator,
    TriggerOperandKind, TriggerOperandValue, TriggerPredicate, TriggerProgram, TriggerStage,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TriggerEditorChannel {
    pub id: CaptureChannelId,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TriggerEditorAction {
    Clear,
    AddStage,
    RemoveStage {
        stage: usize,
    },
    SetStageLogic {
        stage: usize,
        logic: TriggerLogicOperator,
    },
    SetStageInverted {
        stage: usize,
        inverted: bool,
    },
    SetStageCount {
        stage: usize,
        count: Option<TriggerCount>,
    },
    AddDigitalPredicate {
        stage: usize,
        channel: CaptureChannelId,
        condition: SimpleTriggerCondition,
    },
    AddRegisteredPredicate {
        stage: usize,
        predicate: TriggerIdentifier,
    },
    RemovePredicate {
        stage: usize,
        predicate: usize,
    },
    SetDigitalChannel {
        stage: usize,
        predicate: usize,
        channel: CaptureChannelId,
    },
    SetDigitalCondition {
        stage: usize,
        predicate: usize,
        condition: SimpleTriggerCondition,
    },
    SetRegisteredOperand {
        stage: usize,
        predicate: usize,
        operand: TriggerIdentifier,
        value: TriggerOperandValue,
    },
}

pub struct TriggerEditorModel<'a> {
    schema: &'a TriggerEditorSchema,
    channels: &'a [TriggerEditorChannel],
}

impl<'a> TriggerEditorModel<'a> {
    pub const fn new(
        schema: &'a TriggerEditorSchema,
        channels: &'a [TriggerEditorChannel],
    ) -> Self {
        Self { schema, channels }
    }

    pub fn apply(
        &self,
        current: Option<&TriggerProgram>,
        action: TriggerEditorAction,
    ) -> Result<Option<TriggerProgram>, String> {
        if action == TriggerEditorAction::Clear {
            return Ok(None);
        }
        if let Some(current) = current {
            self.schema
                .validate_program(current, &self.channel_ids())
                .map_err(|error| error.to_string())?;
        }
        let mut program = current.cloned().unwrap_or_else(|| {
            TriggerProgram::new(self.schema.id().clone(), self.schema.revision(), Vec::new())
        });
        match action {
            TriggerEditorAction::Clear => unreachable!(),
            TriggerEditorAction::AddStage => {
                if program.stages.len() >= self.schema.maximum_stages() {
                    return Err(format!(
                        "this trigger schema supports at most {} stage(s)",
                        self.schema.maximum_stages()
                    ));
                }
                program.stages.push(self.default_stage()?);
            }
            TriggerEditorAction::RemoveStage { stage } => {
                checked_remove(&mut program.stages, stage, "trigger stage")?;
                if program.stages.is_empty() {
                    return Ok(None);
                }
            }
            TriggerEditorAction::SetStageLogic { stage, logic } => {
                self.stage_mut(&mut program, stage)?.logic = logic;
            }
            TriggerEditorAction::SetStageInverted { stage, inverted } => {
                self.stage_mut(&mut program, stage)?.inverted = inverted;
            }
            TriggerEditorAction::SetStageCount { stage, count } => {
                self.stage_mut(&mut program, stage)?.count = count;
            }
            TriggerEditorAction::AddDigitalPredicate {
                stage,
                channel,
                condition,
            } => {
                self.ensure_predicate_capacity(&program, stage)?;
                self.stage_mut(&mut program, stage)?
                    .predicates
                    .push(TriggerPredicate::Digital { channel, condition });
            }
            TriggerEditorAction::AddRegisteredPredicate { stage, predicate } => {
                self.ensure_predicate_capacity(&program, stage)?;
                let predicate_schema = self
                    .schema
                    .registered_predicates()
                    .iter()
                    .find(|candidate| candidate.id() == &predicate)
                    .ok_or_else(|| {
                        format!("registered trigger predicate '{predicate}' is unknown")
                    })?;
                let operands = self.default_operands(predicate_schema)?;
                self.stage_mut(&mut program, stage)?.predicates.push(
                    TriggerPredicate::Registered {
                        predicate,
                        operands,
                    },
                );
            }
            TriggerEditorAction::RemovePredicate { stage, predicate } => {
                let stage_ref = self.stage_mut(&mut program, stage)?;
                checked_remove(&mut stage_ref.predicates, predicate, "trigger predicate")?;
                if stage_ref.predicates.is_empty() {
                    program.stages.remove(stage);
                    if program.stages.is_empty() {
                        return Ok(None);
                    }
                }
            }
            TriggerEditorAction::SetDigitalChannel {
                stage,
                predicate,
                channel,
            } => {
                let TriggerPredicate::Digital {
                    channel: current, ..
                } = self.predicate_mut(&mut program, stage, predicate)?
                else {
                    return Err("the selected predicate is not a digital condition".into());
                };
                *current = channel;
            }
            TriggerEditorAction::SetDigitalCondition {
                stage,
                predicate,
                condition,
            } => {
                let TriggerPredicate::Digital {
                    condition: current, ..
                } = self.predicate_mut(&mut program, stage, predicate)?
                else {
                    return Err("the selected predicate is not a digital condition".into());
                };
                *current = condition;
            }
            TriggerEditorAction::SetRegisteredOperand {
                stage,
                predicate,
                operand,
                value,
            } => {
                let TriggerPredicate::Registered { operands, .. } =
                    self.predicate_mut(&mut program, stage, predicate)?
                else {
                    return Err("the selected predicate is not registered".into());
                };
                let Some(current) = operands.get_mut(&operand) else {
                    return Err(format!("registered trigger operand '{operand}' is unknown"));
                };
                *current = value;
            }
        }
        self.schema
            .validate_program(&program, &self.channel_ids())
            .map_err(|error| error.to_string())?;
        Ok(Some(program))
    }

    fn channel_ids(&self) -> Vec<CaptureChannelId> {
        self.channels
            .iter()
            .map(|channel| channel.id.clone())
            .collect()
    }

    fn default_stage(&self) -> Result<TriggerStage, String> {
        let predicate = if let (Some(channel), Some(condition)) = (
            self.channels.first(),
            self.schema.digital_conditions().first(),
        ) {
            TriggerPredicate::Digital {
                channel: channel.id.clone(),
                condition: *condition,
            }
        } else if let Some(predicate) = self.schema.registered_predicates().first() {
            TriggerPredicate::Registered {
                predicate: predicate.id().clone(),
                operands: self.default_operands(predicate)?,
            }
        } else {
            return Err("this trigger schema has no predicate available for a new stage".into());
        };
        Ok(TriggerStage {
            predicates: vec![predicate],
            logic: *self
                .schema
                .logic_operators()
                .first()
                .ok_or_else(|| "this trigger schema has no stage logic".to_owned())?,
            inverted: false,
            count: None,
        })
    }

    fn default_operands(
        &self,
        predicate: &RegisteredTriggerPredicateSchema,
    ) -> Result<BTreeMap<TriggerIdentifier, TriggerOperandValue>, String> {
        predicate
            .operands()
            .iter()
            .map(|operand| {
                self.default_operand(operand.kind())
                    .map(|value| (operand.id().clone(), value))
            })
            .collect()
    }

    fn default_operand(&self, kind: &TriggerOperandKind) -> Result<TriggerOperandValue, String> {
        Ok(match kind {
            TriggerOperandKind::Boolean { default } => TriggerOperandValue::Boolean(*default),
            TriggerOperandKind::Unsigned { default, .. } => TriggerOperandValue::Unsigned(*default),
            TriggerOperandKind::Signed { default, .. } => TriggerOperandValue::Signed(*default),
            TriggerOperandKind::DurationNs { default, .. } => {
                TriggerOperandValue::DurationNs(*default)
            }
            TriggerOperandKind::Choice { default, .. } => {
                TriggerOperandValue::Choice(default.clone())
            }
            TriggerOperandKind::Channel { default } => {
                let channel = default
                    .as_ref()
                    .filter(|default| self.channels.iter().any(|channel| channel.id == **default))
                    .cloned()
                    .or_else(|| self.channels.first().map(|channel| channel.id.clone()))
                    .ok_or_else(|| "a channel operand requires an enabled channel".to_owned())?;
                TriggerOperandValue::Channel(channel)
            }
            TriggerOperandKind::Bytes { default, .. } => {
                TriggerOperandValue::Bytes(default.clone())
            }
        })
    }

    fn stage_mut<'program>(
        &self,
        program: &'program mut TriggerProgram,
        stage: usize,
    ) -> Result<&'program mut TriggerStage, String> {
        program
            .stages
            .get_mut(stage)
            .ok_or_else(|| format!("trigger stage {stage} does not exist"))
    }

    fn predicate_mut<'program>(
        &self,
        program: &'program mut TriggerProgram,
        stage: usize,
        predicate: usize,
    ) -> Result<&'program mut TriggerPredicate, String> {
        self.stage_mut(program, stage)?
            .predicates
            .get_mut(predicate)
            .ok_or_else(|| format!("trigger predicate {stage}:{predicate} does not exist"))
    }

    fn ensure_predicate_capacity(
        &self,
        program: &TriggerProgram,
        stage: usize,
    ) -> Result<(), String> {
        let stage = program
            .stages
            .get(stage)
            .ok_or_else(|| format!("trigger stage {stage} does not exist"))?;
        if stage.predicates.len() >= self.schema.maximum_predicates_per_stage() {
            return Err(format!(
                "this trigger schema supports at most {} predicate(s) per stage",
                self.schema.maximum_predicates_per_stage()
            ));
        }
        Ok(())
    }
}

fn checked_remove<T>(values: &mut Vec<T>, index: usize, label: &str) -> Result<T, String> {
    if index >= values.len() {
        return Err(format!("{label} {index} does not exist"));
    }
    Ok(values.remove(index))
}

#[derive(Default)]
pub struct TriggerEditorResponse {
    pub program: Option<Option<TriggerProgram>>,
    pub error: Option<String>,
}

pub struct TriggerEditor<'a> {
    schema: &'a TriggerEditorSchema,
    channels: &'a [TriggerEditorChannel],
    program: Option<&'a TriggerProgram>,
    enabled: bool,
}

impl<'a> TriggerEditor<'a> {
    pub const fn new(
        schema: &'a TriggerEditorSchema,
        channels: &'a [TriggerEditorChannel],
        program: Option<&'a TriggerProgram>,
    ) -> Self {
        Self {
            schema,
            channels,
            program,
            enabled: true,
        }
    }

    pub const fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn show(self, ui: &mut egui::Ui) -> TriggerEditorResponse {
        let mut action = None;
        let validation = self
            .program
            .map(|program| {
                self.schema.validate_program(
                    program,
                    &self
                        .channels
                        .iter()
                        .map(|channel| channel.id.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .transpose();
        if let Err(errors) = validation {
            for diagnostic in errors.diagnostics() {
                ui.colored_label(
                    ui.visuals().error_fg_color,
                    format!("{}: {}", diagnostic.path, diagnostic.message),
                );
            }
            if ui
                .add_enabled(
                    self.enabled,
                    egui::Button::new("Clear incompatible trigger"),
                )
                .clicked()
            {
                action = Some(TriggerEditorAction::Clear);
            }
        } else if let Some(program) = self.program {
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.enabled, egui::Button::new("Clear Trigger"))
                    .clicked()
                {
                    action = Some(TriggerEditorAction::Clear);
                }
                if ui
                    .add_enabled(
                        self.enabled && program.stages.len() < self.schema.maximum_stages(),
                        egui::Button::new("+ Stage"),
                    )
                    .clicked()
                {
                    action = Some(TriggerEditorAction::AddStage);
                }
            });
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (stage_index, stage) in program.stages.iter().enumerate() {
                    ui.group(|ui| {
                        self.show_stage(ui, stage_index, stage, &mut action);
                    });
                    ui.add_space(4.0);
                }
            });
        } else {
            ui.label(egui::RichText::new("Free run — no trigger program").weak());
            if ui
                .add_enabled(self.enabled, egui::Button::new("Add Trigger"))
                .clicked()
            {
                action = Some(TriggerEditorAction::AddStage);
            }
        }

        let Some(action) = action else {
            return TriggerEditorResponse::default();
        };
        match TriggerEditorModel::new(self.schema, self.channels).apply(self.program, action) {
            Ok(program) => TriggerEditorResponse {
                program: Some(program),
                error: None,
            },
            Err(error) => TriggerEditorResponse {
                program: None,
                error: Some(error),
            },
        }
    }

    fn show_stage(
        &self,
        ui: &mut egui::Ui,
        stage_index: usize,
        stage: &TriggerStage,
        action: &mut Option<TriggerEditorAction>,
    ) {
        ui.horizontal(|ui| {
            ui.strong(format!("Stage {}", stage_index + 1));
            ui.add_enabled_ui(self.enabled, |ui| {
                egui::ComboBox::from_id_salt(("trigger-stage-logic", stage_index))
                    .selected_text(logic_label(stage.logic))
                    .show_ui(ui, |ui| {
                        for logic in self.schema.logic_operators() {
                            if ui
                                .selectable_label(*logic == stage.logic, logic_label(*logic))
                                .clicked()
                            {
                                *action = Some(TriggerEditorAction::SetStageLogic {
                                    stage: stage_index,
                                    logic: *logic,
                                });
                            }
                        }
                    });
            });
            if self.schema.supports_stage_inversion() {
                let mut inverted = stage.inverted;
                if ui
                    .add_enabled(self.enabled, egui::Checkbox::new(&mut inverted, "Invert"))
                    .changed()
                {
                    *action = Some(TriggerEditorAction::SetStageInverted {
                        stage: stage_index,
                        inverted,
                    });
                }
            }
            if ui
                .add_enabled(self.enabled, egui::Button::new("Remove"))
                .clicked()
            {
                *action = Some(TriggerEditorAction::RemoveStage { stage: stage_index });
            }
        });
        self.show_count(ui, stage_index, stage, action);
        for (predicate_index, predicate) in stage.predicates.iter().enumerate() {
            ui.horizontal_wrapped(|ui| {
                self.show_predicate(ui, stage_index, predicate_index, predicate, action);
                if ui
                    .add_enabled(self.enabled, egui::Button::new("×"))
                    .on_hover_text("Remove condition")
                    .clicked()
                {
                    *action = Some(TriggerEditorAction::RemovePredicate {
                        stage: stage_index,
                        predicate: predicate_index,
                    });
                }
            });
        }
        let unused_digital_channel = self.channels.iter().find(|channel| {
            !stage.predicates.iter().any(|predicate| {
                matches!(
                    predicate,
                    TriggerPredicate::Digital { channel: used, .. } if *used == channel.id
                )
            })
        });
        let can_add_digital =
            unused_digital_channel.is_some() && !self.schema.digital_conditions().is_empty();
        let has_condition_kind = can_add_digital || !self.schema.registered_predicates().is_empty();
        ui.add_enabled_ui(
            self.enabled
                && has_condition_kind
                && stage.predicates.len() < self.schema.maximum_predicates_per_stage(),
            |ui| {
                ui.menu_button("+ Condition", |ui| {
                    if let (Some(channel), Some(condition)) = (
                        unused_digital_channel,
                        self.schema.digital_conditions().first(),
                    ) && ui.button("Digital condition").clicked()
                    {
                        *action = Some(TriggerEditorAction::AddDigitalPredicate {
                            stage: stage_index,
                            channel: channel.id.clone(),
                            condition: *condition,
                        });
                        ui.close();
                    }
                    for predicate in self.schema.registered_predicates() {
                        if ui.button(predicate.label()).clicked() {
                            *action = Some(TriggerEditorAction::AddRegisteredPredicate {
                                stage: stage_index,
                                predicate: predicate.id().clone(),
                            });
                            ui.close();
                        }
                    }
                });
            },
        );
    }

    fn show_count(
        &self,
        ui: &mut egui::Ui,
        stage_index: usize,
        stage: &TriggerStage,
        action: &mut Option<TriggerEditorAction>,
    ) {
        let Some(capabilities) = self.schema.count_capabilities() else {
            return;
        };
        ui.horizontal(|ui| {
            let mut enabled = stage.count.is_some();
            if ui
                .add_enabled(self.enabled, egui::Checkbox::new(&mut enabled, "Count"))
                .changed()
            {
                let count = enabled.then(|| TriggerCount {
                    mode: capabilities.modes()[0],
                    value: capabilities.minimum(),
                });
                *action = Some(TriggerEditorAction::SetStageCount {
                    stage: stage_index,
                    count,
                });
            }
            let Some(count) = stage.count else {
                return;
            };
            ui.add_enabled_ui(self.enabled, |ui| {
                egui::ComboBox::from_id_salt(("trigger-count-mode", stage_index))
                    .selected_text(count_mode_label(count.mode))
                    .show_ui(ui, |ui| {
                        for mode in capabilities.modes() {
                            if ui
                                .selectable_label(*mode == count.mode, count_mode_label(*mode))
                                .clicked()
                            {
                                *action = Some(TriggerEditorAction::SetStageCount {
                                    stage: stage_index,
                                    count: Some(TriggerCount {
                                        mode: *mode,
                                        value: count.value,
                                    }),
                                });
                            }
                        }
                    });
            });
            let mut value = count.value;
            if ui
                .add_enabled(
                    self.enabled,
                    egui::DragValue::new(&mut value)
                        .range(capabilities.minimum()..=capabilities.maximum())
                        .speed(capabilities.step() as f64),
                )
                .changed()
            {
                *action = Some(TriggerEditorAction::SetStageCount {
                    stage: stage_index,
                    count: Some(TriggerCount {
                        mode: count.mode,
                        value,
                    }),
                });
            }
        });
    }

    fn show_predicate(
        &self,
        ui: &mut egui::Ui,
        stage_index: usize,
        predicate_index: usize,
        predicate: &TriggerPredicate,
        action: &mut Option<TriggerEditorAction>,
    ) {
        match predicate {
            TriggerPredicate::Digital { channel, condition } => {
                ui.add_enabled_ui(self.enabled, |ui| {
                    egui::ComboBox::from_id_salt((
                        "trigger-digital-channel",
                        stage_index,
                        predicate_index,
                    ))
                    .selected_text(channel_label(self.channels, channel))
                    .show_ui(ui, |ui| {
                        for candidate in self.channels {
                            if ui
                                .selectable_label(candidate.id == *channel, &candidate.label)
                                .clicked()
                            {
                                *action = Some(TriggerEditorAction::SetDigitalChannel {
                                    stage: stage_index,
                                    predicate: predicate_index,
                                    channel: candidate.id.clone(),
                                });
                            }
                        }
                    });
                });
                ui.add_enabled_ui(self.enabled, |ui| {
                    egui::ComboBox::from_id_salt((
                        "trigger-digital-condition",
                        stage_index,
                        predicate_index,
                    ))
                    .selected_text(condition_label(*condition))
                    .show_ui(ui, |ui| {
                        for candidate in self.schema.digital_conditions() {
                            if ui
                                .selectable_label(
                                    *candidate == *condition,
                                    condition_label(*candidate),
                                )
                                .clicked()
                            {
                                *action = Some(TriggerEditorAction::SetDigitalCondition {
                                    stage: stage_index,
                                    predicate: predicate_index,
                                    condition: *candidate,
                                });
                            }
                        }
                    });
                });
            }
            TriggerPredicate::Registered {
                predicate,
                operands,
            } => {
                let Some(predicate_schema) = self
                    .schema
                    .registered_predicates()
                    .iter()
                    .find(|candidate| candidate.id() == predicate)
                else {
                    ui.colored_label(ui.visuals().error_fg_color, predicate.as_str());
                    return;
                };
                ui.strong(predicate_schema.label());
                for operand in predicate_schema.operands() {
                    ui.label(operand.label());
                    if let Some(value) = operands.get(operand.id())
                        && let Some(updated) = self.show_operand(
                            ui,
                            (stage_index, predicate_index, operand.id().as_str()),
                            operand.kind(),
                            value,
                        )
                    {
                        *action = Some(TriggerEditorAction::SetRegisteredOperand {
                            stage: stage_index,
                            predicate: predicate_index,
                            operand: operand.id().clone(),
                            value: updated,
                        });
                    }
                }
            }
        }
    }

    fn show_operand(
        &self,
        ui: &mut egui::Ui,
        id: (usize, usize, &str),
        kind: &TriggerOperandKind,
        value: &TriggerOperandValue,
    ) -> Option<TriggerOperandValue> {
        match (kind, value) {
            (TriggerOperandKind::Boolean { .. }, TriggerOperandValue::Boolean(value)) => {
                let mut updated = *value;
                ui.add_enabled(self.enabled, egui::Checkbox::without_text(&mut updated))
                    .changed()
                    .then_some(TriggerOperandValue::Boolean(updated))
            }
            (
                TriggerOperandKind::Unsigned {
                    minimum,
                    maximum,
                    step,
                    ..
                },
                TriggerOperandValue::Unsigned(value),
            ) => unsigned_operand(ui, self.enabled, *minimum, *maximum, *step, *value)
                .map(TriggerOperandValue::Unsigned),
            (
                TriggerOperandKind::DurationNs {
                    minimum,
                    maximum,
                    step,
                    ..
                },
                TriggerOperandValue::DurationNs(value),
            ) => unsigned_operand(ui, self.enabled, *minimum, *maximum, *step, *value)
                .map(TriggerOperandValue::DurationNs),
            (
                TriggerOperandKind::Signed {
                    minimum,
                    maximum,
                    step,
                    ..
                },
                TriggerOperandValue::Signed(value),
            ) => {
                let mut updated = *value;
                ui.add_enabled(
                    self.enabled,
                    egui::DragValue::new(&mut updated)
                        .range(*minimum..=*maximum)
                        .speed(*step as f64),
                )
                .changed()
                .then_some(TriggerOperandValue::Signed(updated))
            }
            (TriggerOperandKind::Choice { choices, .. }, TriggerOperandValue::Choice(value)) => {
                let mut updated = None;
                let selected = choices
                    .iter()
                    .find(|choice| choice.id() == value)
                    .map_or(value.as_str(), |choice| choice.label());
                ui.add_enabled_ui(self.enabled, |ui| {
                    egui::ComboBox::from_id_salt(("trigger-choice", id))
                        .selected_text(selected)
                        .show_ui(ui, |ui| {
                            for choice in choices {
                                if ui
                                    .selectable_label(choice.id() == value, choice.label())
                                    .clicked()
                                {
                                    updated =
                                        Some(TriggerOperandValue::Choice(choice.id().clone()));
                                }
                            }
                        });
                });
                updated
            }
            (TriggerOperandKind::Channel { .. }, TriggerOperandValue::Channel(value)) => {
                let mut updated = None;
                ui.add_enabled_ui(self.enabled, |ui| {
                    egui::ComboBox::from_id_salt(("trigger-channel", id))
                        .selected_text(channel_label(self.channels, value))
                        .show_ui(ui, |ui| {
                            for channel in self.channels {
                                if ui
                                    .selectable_label(channel.id == *value, &channel.label)
                                    .clicked()
                                {
                                    updated =
                                        Some(TriggerOperandValue::Channel(channel.id.clone()));
                                }
                            }
                        });
                });
                updated
            }
            (
                TriggerOperandKind::Bytes {
                    minimum_length,
                    maximum_length,
                    ..
                },
                TriggerOperandValue::Bytes(value),
            ) => {
                let memory_id = ui.make_persistent_id(("trigger-bytes", id));
                let canonical = format_bytes(value);
                let mut text = ui
                    .data(|data| data.get_temp::<String>(memory_id))
                    .unwrap_or(canonical.clone());
                let response = ui.add_enabled(
                    self.enabled,
                    egui::TextEdit::singleline(&mut text).desired_width(120.0),
                );
                if response.changed() {
                    ui.data_mut(|data| data.insert_temp(memory_id, text.clone()));
                    parse_bytes(&text)
                        .filter(|bytes| {
                            bytes.len() >= *minimum_length && bytes.len() <= *maximum_length
                        })
                        .map(TriggerOperandValue::Bytes)
                } else {
                    if !response.has_focus() && text != canonical {
                        ui.data_mut(|data| data.remove::<String>(memory_id));
                    }
                    None
                }
            }
            _ => {
                ui.colored_label(ui.visuals().error_fg_color, "wrong operand type");
                None
            }
        }
    }
}

fn unsigned_operand(
    ui: &mut egui::Ui,
    enabled: bool,
    minimum: u64,
    maximum: u64,
    step: u64,
    value: u64,
) -> Option<u64> {
    let mut updated = value;
    ui.add_enabled(
        enabled,
        egui::DragValue::new(&mut updated)
            .range(minimum..=maximum)
            .speed(step as f64),
    )
    .changed()
    .then_some(updated)
}

fn channel_label<'a>(channels: &'a [TriggerEditorChannel], id: &CaptureChannelId) -> &'a str {
    channels
        .iter()
        .find(|channel| channel.id == *id)
        .map_or("Unknown channel", |channel| channel.label.as_str())
}

const fn logic_label(logic: TriggerLogicOperator) -> &'static str {
    match logic {
        TriggerLogicOperator::And => "AND",
        TriggerLogicOperator::Or => "OR",
        TriggerLogicOperator::Xor => "XOR",
        TriggerLogicOperator::Nand => "NAND",
        TriggerLogicOperator::Nor => "NOR",
    }
}

const fn count_mode_label(mode: TriggerCountMode) -> &'static str {
    match mode {
        TriggerCountMode::Occurrences => "Occurrences",
        TriggerCountMode::Consecutive => "Consecutive",
    }
}

const fn condition_label(condition: SimpleTriggerCondition) -> &'static str {
    match condition {
        SimpleTriggerCondition::Ignore => "Ignore",
        SimpleTriggerCondition::Low => "Low",
        SimpleTriggerCondition::High => "High",
        SimpleTriggerCondition::Rising => "Rising edge",
        SimpleTriggerCondition::Falling => "Falling edge",
        SimpleTriggerCondition::Either => "Either edge",
    }
}

fn format_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_bytes(value: &str) -> Option<Vec<u8>> {
    value
        .split_whitespace()
        .map(|byte| u8::from_str_radix(byte, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use signal_processing::{
        RegisteredTriggerPredicateSchema, TriggerChoice, TriggerCountCapabilities,
        TriggerOperandSchema,
    };

    use super::*;

    fn id(value: &str) -> TriggerIdentifier {
        TriggerIdentifier::new(value).unwrap()
    }

    fn schema() -> TriggerEditorSchema {
        TriggerEditorSchema::new(
            id("test.editor"),
            1,
            2,
            4,
            vec![TriggerLogicOperator::And, TriggerLogicOperator::Or],
        )
        .unwrap()
        .with_digital_conditions(vec![
            SimpleTriggerCondition::High,
            SimpleTriggerCondition::Rising,
        ])
        .unwrap()
        .with_stage_inversion(true)
        .with_count(
            TriggerCountCapabilities::new(vec![TriggerCountMode::Occurrences], 1, 9, 1).unwrap(),
        )
        .with_registered_predicates(vec![
            RegisteredTriggerPredicateSchema::new(
                id("test.sequence"),
                "Sequence",
                vec![
                    TriggerOperandSchema::new(
                        id("channel"),
                        "Channel",
                        TriggerOperandKind::Channel { default: None },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("value"),
                        "Value",
                        TriggerOperandKind::Unsigned {
                            minimum: 0,
                            maximum: 255,
                            step: 1,
                            default: 0,
                        },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("enabled"),
                        "Enabled",
                        TriggerOperandKind::Boolean { default: true },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("offset"),
                        "Offset",
                        TriggerOperandKind::Signed {
                            minimum: -8,
                            maximum: 8,
                            step: 2,
                            default: 0,
                        },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("duration"),
                        "Duration",
                        TriggerOperandKind::DurationNs {
                            minimum: 100,
                            maximum: 1_000,
                            step: 100,
                            default: 100,
                        },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("edge"),
                        "Edge",
                        TriggerOperandKind::Choice {
                            choices: vec![
                                TriggerChoice::new(id("rise"), "Rise").unwrap(),
                                TriggerChoice::new(id("fall"), "Fall").unwrap(),
                            ],
                            default: id("rise"),
                        },
                    )
                    .unwrap(),
                    TriggerOperandSchema::new(
                        id("bytes"),
                        "Bytes",
                        TriggerOperandKind::Bytes {
                            minimum_length: 1,
                            maximum_length: 4,
                            default: vec![0],
                        },
                    )
                    .unwrap(),
                ],
            )
            .unwrap(),
        ])
        .unwrap()
    }

    fn channels() -> Vec<TriggerEditorChannel> {
        vec![
            TriggerEditorChannel {
                id: CaptureChannelId::new("bank-a:7"),
                label: "A7".into(),
            },
            TriggerEditorChannel {
                id: CaptureChannelId::new("bank-z:41"),
                label: "Z41".into(),
            },
        ]
    }

    #[test]
    fn neutral_actions_build_stages_counts_and_registered_operands() {
        let schema = schema();
        let channels = channels();
        let model = TriggerEditorModel::new(&schema, &channels);
        let mut program = model.apply(None, TriggerEditorAction::AddStage).unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::AddRegisteredPredicate {
                    stage: 0,
                    predicate: id("test.sequence"),
                },
            )
            .unwrap();
        for (operand, value) in [
            (id("enabled"), TriggerOperandValue::Boolean(false)),
            (id("offset"), TriggerOperandValue::Signed(-2)),
            (id("duration"), TriggerOperandValue::DurationNs(300)),
            (id("edge"), TriggerOperandValue::Choice(id("fall"))),
            (
                id("channel"),
                TriggerOperandValue::Channel(channels[1].id.clone()),
            ),
            (id("bytes"), TriggerOperandValue::Bytes(vec![0x5a, 0xa5])),
        ] {
            program = model
                .apply(
                    program.as_ref(),
                    TriggerEditorAction::SetRegisteredOperand {
                        stage: 0,
                        predicate: 1,
                        operand,
                        value,
                    },
                )
                .unwrap();
        }
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetStageCount {
                    stage: 0,
                    count: Some(TriggerCount {
                        mode: TriggerCountMode::Occurrences,
                        value: 4,
                    }),
                },
            )
            .unwrap();
        assert!(
            model
                .apply(
                    program.as_ref(),
                    TriggerEditorAction::AddDigitalPredicate {
                        stage: 0,
                        channel: channels[0].id.clone(),
                        condition: SimpleTriggerCondition::Rising,
                    },
                )
                .unwrap_err()
                .contains("more than once")
        );
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetRegisteredOperand {
                    stage: 0,
                    predicate: 1,
                    operand: id("value"),
                    value: TriggerOperandValue::Unsigned(0x5a),
                },
            )
            .unwrap();
        program = model
            .apply(program.as_ref(), TriggerEditorAction::AddStage)
            .unwrap();

        let program = program.unwrap();
        assert_eq!(program.stages.len(), 2);
        assert_eq!(program.stages[0].count.unwrap().value, 4);
        let TriggerPredicate::Registered { operands, .. } = &program.stages[0].predicates[1] else {
            panic!("second predicate should be registered");
        };
        assert_eq!(operands[&id("value")], TriggerOperandValue::Unsigned(0x5a));
        assert_eq!(
            operands[&id("bytes")],
            TriggerOperandValue::Bytes(vec![0x5a, 0xa5])
        );
        schema
            .validate_program(
                &program,
                &channels
                    .iter()
                    .map(|channel| channel.id.clone())
                    .collect::<Vec<_>>(),
            )
            .unwrap();

        let without_registered = model
            .apply(
                Some(&program),
                TriggerEditorAction::RemovePredicate {
                    stage: 0,
                    predicate: 1,
                },
            )
            .unwrap();
        let without_count = model
            .apply(
                without_registered.as_ref(),
                TriggerEditorAction::SetStageCount {
                    stage: 0,
                    count: None,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(without_count.stages[0].predicates.len(), 1);
        assert!(without_count.stages[0].count.is_none());
    }

    #[test]
    fn neutral_actions_enforce_schema_limits_and_clear_invalid_programs() {
        let schema = schema();
        let channels = channels();
        let model = TriggerEditorModel::new(&schema, &channels);
        let first = model.apply(None, TriggerEditorAction::AddStage).unwrap();
        let second = model
            .apply(first.as_ref(), TriggerEditorAction::AddStage)
            .unwrap();
        assert!(
            model
                .apply(second.as_ref(), TriggerEditorAction::AddStage)
                .unwrap_err()
                .contains("at most 2")
        );

        let mut incompatible = second.unwrap();
        incompatible.schema_revision = 99;
        assert!(
            model
                .apply(Some(&incompatible), TriggerEditorAction::AddStage)
                .is_err()
        );
        assert_eq!(
            model
                .apply(Some(&incompatible), TriggerEditorAction::Clear)
                .unwrap(),
            None
        );
    }

    #[test]
    fn neutral_actions_edit_and_remove_digital_predicates_and_stages() {
        let schema = schema();
        let channels = channels();
        let model = TriggerEditorModel::new(&schema, &channels);
        let mut program = model.apply(None, TriggerEditorAction::AddStage).unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetDigitalChannel {
                    stage: 0,
                    predicate: 0,
                    channel: channels[1].id.clone(),
                },
            )
            .unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetDigitalCondition {
                    stage: 0,
                    predicate: 0,
                    condition: SimpleTriggerCondition::Rising,
                },
            )
            .unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::AddDigitalPredicate {
                    stage: 0,
                    channel: channels[0].id.clone(),
                    condition: SimpleTriggerCondition::High,
                },
            )
            .unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetStageLogic {
                    stage: 0,
                    logic: TriggerLogicOperator::Or,
                },
            )
            .unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::SetStageInverted {
                    stage: 0,
                    inverted: true,
                },
            )
            .unwrap();
        program = model
            .apply(
                program.as_ref(),
                TriggerEditorAction::RemovePredicate {
                    stage: 0,
                    predicate: 1,
                },
            )
            .unwrap();
        let program_ref = program.as_ref().unwrap();
        assert_eq!(program_ref.stages[0].logic, TriggerLogicOperator::Or);
        assert!(program_ref.stages[0].inverted);
        assert_eq!(program_ref.stages[0].predicates.len(), 1);

        assert_eq!(
            model
                .apply(
                    program.as_ref(),
                    TriggerEditorAction::RemoveStage { stage: 0 },
                )
                .unwrap(),
            None
        );
    }

    #[test]
    fn byte_values_use_bounded_hex_pairs() {
        assert_eq!(parse_bytes("00 5a FF"), Some(vec![0, 0x5a, 0xff]));
        assert_eq!(parse_bytes("5"), Some(vec![5]));
        assert_eq!(parse_bytes("xyz"), None);
        assert_eq!(format_bytes(&[0, 0x5a, 0xff]), "00 5A FF");
    }
}
