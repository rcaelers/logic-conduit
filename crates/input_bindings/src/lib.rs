//! Application-neutral input binding configuration and lookup.
//!
//! Contexts and actions are deliberately opaque strings. This crate knows
//! nothing about the application that supplies them or the widgets that use
//! them.

use std::collections::{HashMap, HashSet};
use std::fmt;

use egui::{Key, KeyboardShortcut, Modifiers, PointerButton, Ui};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BindingFile {
    pub bindings: Vec<Binding>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Binding {
    pub context: String,
    pub action: String,
    pub label: String,
    #[serde(default)]
    pub modifiers: ModifierSpec,
    #[serde(flatten)]
    pub trigger: Trigger,
    #[serde(default)]
    pub menu: bool,
    #[serde(default = "default_true")]
    pub status: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ModifierSpec {
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub control: bool,
    #[serde(default)]
    pub shift: bool,
    /// The platform command modifier: Command on macOS and Control elsewhere.
    #[serde(default)]
    pub command: bool,
}

impl ModifierSpec {
    pub fn to_egui(self) -> Modifiers {
        Modifiers {
            alt: self.alt,
            ctrl: self.control,
            shift: self.shift,
            mac_cmd: cfg!(target_os = "macos") && self.command,
            command: self.command,
        }
    }

    pub fn matches(self, actual: Modifiers) -> bool {
        let expected_control = self.control || (!cfg!(target_os = "macos") && self.command);
        let expected_mac_command = cfg!(target_os = "macos") && self.command;
        self.alt == actual.alt
            && expected_control == actual.ctrl
            && self.shift == actual.shift
            && expected_mac_command == actual.mac_cmd
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "input", rename_all = "snake_case")]
pub enum Trigger {
    Key {
        key: String,
    },
    Pointer {
        button: PointerButtonName,
        #[serde(default)]
        gesture: PointerGesture,
    },
    Wheel {
        axis: WheelAxis,
    },
    Zoom,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PointerButtonName {
    Primary,
    Middle,
    Secondary,
    Extra1,
    Extra2,
}

impl PointerButtonName {
    pub fn to_egui(self) -> PointerButton {
        match self {
            Self::Primary => PointerButton::Primary,
            Self::Middle => PointerButton::Middle,
            Self::Secondary => PointerButton::Secondary,
            Self::Extra1 => PointerButton::Extra1,
            Self::Extra2 => PointerButton::Extra2,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PointerGesture {
    #[default]
    Click,
    DoubleClick,
    Drag,
    Press,
    Release,
    Hold,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum WheelAxis {
    Horizontal,
    Vertical,
    Both,
}

#[derive(Debug, Error)]
pub enum BindingError {
    #[error("invalid input binding JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("binding {context}.{action} uses unknown key {key:?}")]
    UnknownKey {
        context: String,
        action: String,
        key: String,
    },
    #[error("conflicting input bindings in {context}: {first} and {second}")]
    ConflictingTrigger {
        context: String,
        first: String,
        second: String,
    },
}

#[derive(Debug, Clone)]
pub struct InputBindings {
    bindings: Vec<Binding>,
    by_action: HashMap<(String, String), Vec<usize>>,
}

impl InputBindings {
    pub fn from_json(json: &str) -> Result<Self, BindingError> {
        let file: BindingFile = serde_json::from_str(json)?;
        Self::new(file.bindings)
    }

    pub fn new(bindings: Vec<Binding>) -> Result<Self, BindingError> {
        let mut by_action = HashMap::new();
        let mut triggers = HashMap::<(String, TriggerIdentity), String>::new();
        for (index, binding) in bindings.iter().enumerate() {
            if let Trigger::Key { key } = &binding.trigger
                && parse_key(key).is_none()
            {
                return Err(BindingError::UnknownKey {
                    context: binding.context.clone(),
                    action: binding.action.clone(),
                    key: key.clone(),
                });
            }
            let action_key = (binding.context.clone(), binding.action.clone());
            by_action
                .entry(action_key)
                .or_insert_with(Vec::new)
                .push(index);
            let trigger_key = (
                binding.context.clone(),
                TriggerIdentity::from_binding(binding),
            );
            if let Some(first) = triggers.insert(trigger_key, binding.action.clone()) {
                return Err(BindingError::ConflictingTrigger {
                    context: binding.context.clone(),
                    first,
                    second: binding.action.clone(),
                });
            }
        }
        Ok(Self {
            bindings,
            by_action,
        })
    }

    pub fn binding(&self, contexts: &[&str], action: &str) -> Option<&Binding> {
        contexts.iter().find_map(|context| {
            self.by_action
                .get(&(String::from(*context), String::from(action)))
                .and_then(|indices| indices.first())
                .map(|index| &self.bindings[*index])
        })
    }

    pub fn bindings(&self, contexts: &[&str], action: &str) -> Vec<&Binding> {
        contexts
            .iter()
            .find_map(|context| {
                self.by_action
                    .get(&(String::from(*context), String::from(action)))
                    .map(|indices| indices.iter().map(|index| &self.bindings[*index]).collect())
            })
            .unwrap_or_default()
    }

    pub fn shortcut(&self, contexts: &[&str], action: &str) -> Option<KeyboardShortcut> {
        let binding = self.binding(contexts, action)?;
        let Trigger::Key { key } = &binding.trigger else {
            return None;
        };
        Some(KeyboardShortcut::new(
            binding.modifiers.to_egui(),
            parse_key(key)?,
        ))
    }

    pub fn consume_shortcut(&self, ui: &mut Ui, contexts: &[&str], action: &str) -> bool {
        self.bindings(contexts, action).into_iter().any(|binding| {
            let Trigger::Key { key } = &binding.trigger else {
                return false;
            };
            let Some(key) = parse_key(key) else {
                return false;
            };
            let shortcut = KeyboardShortcut::new(binding.modifiers.to_egui(), key);
            ui.input_mut(|input| input.consume_shortcut(&shortcut))
        })
    }

    pub fn consume_shortcut_ctx(
        &self,
        context: &egui::Context,
        contexts: &[&str],
        action: &str,
    ) -> bool {
        self.bindings(contexts, action).into_iter().any(|binding| {
            let Trigger::Key { key } = &binding.trigger else {
                return false;
            };
            let Some(key) = parse_key(key) else {
                return false;
            };
            let shortcut = KeyboardShortcut::new(binding.modifiers.to_egui(), key);
            context.input_mut(|input| input.consume_shortcut(&shortcut))
        })
    }

    pub fn pointer_button(&self, contexts: &[&str], action: &str) -> Option<PointerButton> {
        let Trigger::Pointer { button, .. } = self.binding(contexts, action)?.trigger else {
            return None;
        };
        Some(button.to_egui())
    }

    pub fn pointer_gesture(&self, contexts: &[&str], action: &str) -> Option<PointerGesture> {
        let Trigger::Pointer { gesture, .. } = self.binding(contexts, action)?.trigger else {
            return None;
        };
        Some(gesture)
    }

    pub fn pointer_trigger(
        &self,
        contexts: &[&str],
        action: &str,
        modifiers: Modifiers,
    ) -> Option<(PointerButton, PointerGesture)> {
        self.bindings(contexts, action)
            .into_iter()
            .find_map(|binding| {
                if !binding.modifiers.matches(modifiers) {
                    return None;
                }
                let Trigger::Pointer { button, gesture } = binding.trigger else {
                    return None;
                };
                Some((button.to_egui(), gesture))
            })
    }

    /// Returns effective bindings in context-precedence order. An action in a
    /// more specific context shadows the same action in a later context.
    /// Menu-visible shortcuts are intentionally omitted from status hints.
    pub fn status_bindings(&self, contexts: &[&str], modifiers: Modifiers) -> Vec<&Binding> {
        let mut seen = HashSet::new();
        let mut seen_triggers = HashSet::new();
        let mut result = Vec::new();
        for context in contexts {
            for binding in &self.bindings {
                if binding.context == *context
                    && binding.modifiers.matches(modifiers)
                    && seen.insert(binding.action.as_str())
                    && seen_triggers.insert(TriggerIdentity::from_binding(binding))
                    && binding.status
                    && !binding.menu
                {
                    result.push(binding);
                }
            }
        }
        result.sort_by(|left, right| match (&left.trigger, &right.trigger) {
            (Trigger::Key { key: left }, Trigger::Key { key: right }) => {
                compare_key_names(left, right)
            }
            (Trigger::Key { .. }, _) => std::cmp::Ordering::Greater,
            (_, Trigger::Key { .. }) => std::cmp::Ordering::Less,
            _ => std::cmp::Ordering::Equal,
        });
        result
    }
}

fn compare_key_names(left: &str, right: &str) -> std::cmp::Ordering {
    let left = KeySortKey::new(left);
    let right = KeySortKey::new(right);
    left.group
        .cmp(&right.group)
        .then_with(|| left.function_number.cmp(&right.function_number))
        .then_with(|| left.name.cmp(&right.name))
}

struct KeySortKey {
    group: u8,
    function_number: u16,
    name: String,
}

impl KeySortKey {
    fn new(key: &str) -> Self {
        let name = key.to_ascii_lowercase();
        let is_regular = name.len() == 1 && name.as_bytes()[0].is_ascii_alphanumeric();
        let function_number = name
            .strip_prefix('f')
            .and_then(|number| number.parse().ok());
        let group = if is_regular {
            0
        } else if function_number.is_some() {
            1
        } else {
            2
        };
        Self {
            group,
            function_number: function_number.unwrap_or_default(),
            name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TriggerIdentity {
    trigger: String,
    modifiers: ModifierSpec,
}

impl TriggerIdentity {
    fn from_binding(binding: &Binding) -> Self {
        Self {
            trigger: match &binding.trigger {
                Trigger::Key { key } => format!("key:{}", key.to_ascii_lowercase()),
                Trigger::Pointer { button, gesture } => format!("pointer:{button:?}:{gesture:?}"),
                Trigger::Wheel { axis } => format!("wheel:{axis:?}"),
                Trigger::Zoom => "zoom".to_owned(),
            },
            modifiers: binding.modifiers,
        }
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.trigger {
            Trigger::Key { key } => {
                let shortcut = KeyboardShortcut::new(
                    self.modifiers.to_egui(),
                    parse_key(key).ok_or(fmt::Error)?,
                );
                write!(
                    formatter,
                    "{}",
                    shortcut.format(&egui::ModifierNames::NAMES, cfg!(target_os = "macos"))
                )
            }
            Trigger::Pointer { button, .. } => write!(formatter, "{button:?}"),
            Trigger::Wheel { .. } => write!(formatter, "Wheel"),
            Trigger::Zoom => write!(formatter, "Zoom"),
        }
    }
}

fn parse_key(name: &str) -> Option<Key> {
    Some(match name.to_ascii_lowercase().as_str() {
        "a" => Key::A,
        "b" => Key::B,
        "c" => Key::C,
        "d" => Key::D,
        "e" => Key::E,
        "f" => Key::F,
        "g" => Key::G,
        "h" => Key::H,
        "i" => Key::I,
        "j" => Key::J,
        "k" => Key::K,
        "l" => Key::L,
        "m" => Key::M,
        "n" => Key::N,
        "o" => Key::O,
        "p" => Key::P,
        "q" => Key::Q,
        "r" => Key::R,
        "s" => Key::S,
        "t" => Key::T,
        "u" => Key::U,
        "v" => Key::V,
        "w" => Key::W,
        "x" => Key::X,
        "y" => Key::Y,
        "z" => Key::Z,
        "0" => Key::Num0,
        "1" => Key::Num1,
        "2" => Key::Num2,
        "3" => Key::Num3,
        "4" => Key::Num4,
        "5" => Key::Num5,
        "6" => Key::Num6,
        "7" => Key::Num7,
        "8" => Key::Num8,
        "9" => Key::Num9,
        "arrow_down" => Key::ArrowDown,
        "arrow_left" => Key::ArrowLeft,
        "arrow_right" => Key::ArrowRight,
        "arrow_up" => Key::ArrowUp,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "end" => Key::End,
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        "home" => Key::Home,
        "insert" => Key::Insert,
        "page_down" => Key::PageDown,
        "page_up" => Key::PageUp,
        "period" => Key::Period,
        "space" => Key::Space,
        "tab" => Key::Tab,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specific_context_shadows_global_and_menu_bindings_are_not_hints() {
        let manager = InputBindings::from_json(r#"{
          "bindings": [
            {"context":"panel","action":"pick","label":"Pick", "input":"pointer","button":"primary"},
            {"context":"global","action":"pick","label":"Global pick", "input":"pointer","button":"secondary"},
            {"context":"global","action":"save","label":"Save", "input":"key","key":"s","modifiers":{"command":true},"menu":true}
          ]
        }"#).unwrap();
        let hints = manager.status_bindings(&["panel", "global"], Modifiers::NONE);
        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].label, "Pick");
    }

    #[test]
    fn rejects_conflicting_triggers_in_one_context() {
        let error = InputBindings::from_json(
            r#"{"bindings":[
          {"context":"c","action":"one","label":"One","input":"key","key":"a"},
          {"context":"c","action":"two","label":"Two","input":"key","key":"a"}
        ]}"#,
        )
        .unwrap_err();
        assert!(matches!(error, BindingError::ConflictingTrigger { .. }));
    }

    #[test]
    fn status_hints_group_and_sort_keys_after_pointer_inputs() {
        let manager = InputBindings::from_json(
            r#"{"bindings":[
              {"context":"c","action":"z","label":"Zed","input":"key","key":"z"},
              {"context":"c","action":"home","label":"Home","input":"key","key":"home"},
              {"context":"c","action":"f10","label":"Function 10","input":"key","key":"f10"},
              {"context":"c","action":"secondary","label":"Options","input":"pointer","button":"secondary"},
              {"context":"c","action":"arrow_up","label":"Arrow Up","input":"key","key":"arrow_up"},
              {"context":"c","action":"a","label":"Add","input":"key","key":"a"},
              {"context":"c","action":"f2","label":"Function 2","input":"key","key":"f2"},
              {"context":"c","action":"primary","label":"Select","input":"pointer","button":"primary"}
            ]}"#,
        )
        .unwrap();

        let labels: Vec<_> = manager
            .status_bindings(&["c"], Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(
            labels,
            [
                "Options",
                "Select",
                "Add",
                "Zed",
                "Function 2",
                "Function 10",
                "Arrow Up",
                "Home",
            ]
        );
    }

    #[test]
    fn specific_context_shadows_the_same_trigger_in_parent_context() {
        let manager = InputBindings::from_json(
            r#"{"bindings":[
              {"context":"cursor","action":"move_cursor","label":"Move Cursor","input":"pointer","button":"primary","gesture":"drag"},
              {"context":"viewer","action":"pan","label":"Pan View","input":"pointer","button":"primary","gesture":"drag"}
            ]}"#,
        )
        .unwrap();

        let labels: Vec<_> = manager
            .status_bindings(&["cursor", "viewer"], Modifiers::NONE)
            .into_iter()
            .map(|binding| binding.label.as_str())
            .collect();
        assert_eq!(labels, ["Move Cursor"]);
    }
}
