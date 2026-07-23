//! Stable collected-payload identities stored with graph subscriptions.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use node_graph::{GraphState, NodeId, NodeKind, SocketDirection, SocketId};

use super::graph::{BuilderRegistry, resolved_wire_endpoints};

const PAYLOAD_SUBSCRIPTIONS_EXTENSION: &str = "logic_analyzer_graph.payload_subscriptions";
const PAYLOAD_SUBSCRIPTIONS_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
enum SavedSubscriptionTarget {
    ShowInView { node: NodeId, output: usize },
    ViewerInput { node: NodeId, input: usize },
}

impl SavedSubscriptionTarget {
    fn warning_node(&self) -> NodeId {
        match self {
            Self::ShowInView { node, .. } | Self::ViewerInput { node, .. } => *node,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct SavedPayloadSubscription {
    target: SavedSubscriptionTarget,
    payload: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct SavedPayloadSubscriptions {
    version: u32,
    subscriptions: Vec<SavedPayloadSubscription>,
}

/// User-visible compatibility information produced while loading or preparing
/// a graph document.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraphCompatibilityWarning {
    pub node: Option<NodeId>,
    pub message: String,
}

struct DiscoveredSubscription {
    target: SavedSubscriptionTarget,
    label: String,
    current_payload: Option<String>,
}

/// Reconciles saved Viewer and `show_in_view` selections with the payload
/// contracts registered by the current application.
///
/// Documents predating the manifest are upgraded in place. Existing stable
/// identities are retained when their plugin is unavailable, so saving the
/// graph never silently erases the information needed to restore it later.
pub fn synchronize_payload_subscriptions(
    graph: &mut GraphState,
    registry: &BuilderRegistry,
) -> Result<Vec<GraphCompatibilityWarning>, serde_json::Error> {
    let mut warnings = Vec::new();
    let saved = match graph.extension::<SavedPayloadSubscriptions>(PAYLOAD_SUBSCRIPTIONS_EXTENSION)
    {
        Ok(saved) => saved,
        Err(error) => {
            warnings.push(GraphCompatibilityWarning {
                node: None,
                message: format!("Ignored an invalid saved payload-subscription manifest: {error}"),
            });
            None
        }
    };
    let legacy = saved.is_none();
    if let Some(saved) = &saved
        && saved.version != PAYLOAD_SUBSCRIPTIONS_VERSION
    {
        warnings.push(GraphCompatibilityWarning {
            node: None,
            message: format!(
                "Payload-subscription manifest version {} is newer than supported version {}; preserved known lane selections",
                saved.version, PAYLOAD_SUBSCRIPTIONS_VERSION
            ),
        });
    }

    let previous: HashMap<_, _> = saved
        .into_iter()
        .flat_map(|saved| saved.subscriptions)
        .map(|subscription| (subscription.target, subscription.payload))
        .collect();
    let discovered = discover_subscriptions(graph, registry);
    let mut subscriptions = Vec::with_capacity(discovered.len());
    let mut migrated = 0usize;

    for discovered in discovered {
        let previous_payload = previous.get(&discovered.target);
        let payload = match (&discovered.current_payload, previous_payload) {
            (Some(current), Some(previous)) if current != previous => {
                warnings.push(GraphCompatibilityWarning {
                    node: Some(discovered.target.warning_node()),
                    message: format!(
                        "{} was saved for payload '{}' but now provides '{}'; the current registered presentation is used",
                        discovered.label, previous, current
                    ),
                });
                current.clone()
            }
            (Some(current), _) => {
                if legacy || previous_payload.is_none() {
                    migrated += 1;
                }
                current.clone()
            }
            (None, Some(previous)) => {
                let message = if registry
                    .collected_payloads()
                    .descriptor_by_stable_id(previous)
                    .is_none()
                {
                    format!(
                        "{} needs payload '{}', but that payload is not registered; install or enable its plugin",
                        discovered.label, previous
                    )
                } else if !registry.has_payload_subscription(previous) {
                    format!(
                        "{} needs payload '{}', but its collection/presentation subscription is not registered",
                        discovered.label, previous
                    )
                } else {
                    format!(
                        "{} could not resolve its saved payload '{}' from the current source output",
                        discovered.label, previous
                    )
                };
                warnings.push(GraphCompatibilityWarning {
                    node: Some(discovered.target.warning_node()),
                    message,
                });
                previous.clone()
            }
            (None, None) => {
                warnings.push(GraphCompatibilityWarning {
                    node: Some(discovered.target.warning_node()),
                    message: format!(
                        "{} has no registered collection/presentation contract and could not be migrated",
                        discovered.label
                    ),
                });
                continue;
            }
        };
        subscriptions.push(SavedPayloadSubscription {
            target: discovered.target,
            payload,
        });
    }

    if migrated > 0 {
        warnings.push(GraphCompatibilityWarning {
            node: None,
            message: format!(
                "Migrated {migrated} legacy Viewer lane selection(s) to stable payload identities; their visual presentation was preserved"
            ),
        });
    }

    if subscriptions.is_empty() {
        graph.remove_extension(PAYLOAD_SUBSCRIPTIONS_EXTENSION);
    } else {
        graph.set_extension(
            PAYLOAD_SUBSCRIPTIONS_EXTENSION,
            SavedPayloadSubscriptions {
                version: PAYLOAD_SUBSCRIPTIONS_VERSION,
                subscriptions,
            },
        )?;
    }
    Ok(warnings)
}

fn discover_subscriptions(
    graph: &GraphState,
    registry: &BuilderRegistry,
) -> Vec<DiscoveredSubscription> {
    let mut discovered = Vec::new();
    for (&node_id, node) in &graph.nodes {
        if node.kind != NodeKind::Regular {
            continue;
        }
        for (output_index, output) in node.outputs.iter().enumerate() {
            if output.visible && output.view_selectable && output.show_in_view {
                discovered.push(discover_subscription(
                    graph,
                    registry,
                    SavedSubscriptionTarget::ShowInView {
                        node: node_id,
                        output: output_index,
                    },
                    SocketId {
                        node: node_id,
                        index: output_index,
                        direction: SocketDirection::Output,
                    },
                    format!("View selection '{}.{}'", node.title, output.name),
                ));
            }
        }
    }

    let resolved_sources: HashMap<_, _> = resolved_wire_endpoints(graph)
        .into_iter()
        .map(|(from, to)| (to, from))
        .collect();
    for connection in &graph.connections {
        let to = connection.to;
        let Some(target) = graph.nodes.get(&to.node) else {
            continue;
        };
        if !registry
            .get(target.def_name())
            .is_some_and(|builder| builder.is_data_subscription())
        {
            continue;
        }
        let input_name = target
            .inputs
            .get(to.index)
            .map(|input| input.name.as_str())
            .unwrap_or("?");
        discovered.push(discover_subscription(
            graph,
            registry,
            SavedSubscriptionTarget::ViewerInput {
                node: to.node,
                input: to.index,
            },
            resolved_sources
                .get(&to)
                .copied()
                .unwrap_or(connection.from),
            format!("Viewer input '{}.{}'", target.title, input_name),
        ));
    }
    discovered.sort_by_key(|subscription| match subscription.target {
        SavedSubscriptionTarget::ShowInView { node, output } => (node.0, 0, output),
        SavedSubscriptionTarget::ViewerInput { node, input } => (node.0, 1, input),
    });
    discovered
}

fn discover_subscription(
    graph: &GraphState,
    registry: &BuilderRegistry,
    target: SavedSubscriptionTarget,
    source: SocketId,
    label: String,
) -> DiscoveredSubscription {
    let current_payload = graph
        .nodes
        .get(&source.node)
        .filter(|node| node.kind == NodeKind::Regular)
        .and_then(|node| {
            let builder = registry.get(node.def_name())?;
            let output = node.outputs.get(source.index)?;
            builder
                .offered_kinds(output, &node.state)
                .into_iter()
                .find_map(|kind| {
                    registry
                        .payload_subscription_stable_id(kind)
                        .map(str::to_owned)
                })
        });
    DiscoveredSubscription {
        target,
        label,
        current_payload,
    }
}

#[cfg(test)]
mod saved_graph_tests {
    use node_graph::{NodeGraphWidget, SocketDirection, SocketId};

    use super::*;
    use crate::nodes;

    #[test]
    fn legacy_builtin_view_selections_gain_stable_payload_identities() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("Binary Decoder", egui::Pos2::ZERO)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[0].show_in_view = true;
        let registry = BuilderRegistry::standard();

        let warnings = synchronize_payload_subscriptions(widget.graph_mut(), &registry).unwrap();
        let saved: SavedPayloadSubscriptions = widget
            .graph()
            .extension(PAYLOAD_SUBSCRIPTIONS_EXTENSION)
            .unwrap()
            .unwrap();

        assert_eq!(saved.subscriptions.len(), 1);
        assert_eq!(saved.subscriptions[0].payload, "org.logicconduit.word/v1");
        assert!(warnings.iter().any(|warning| {
            warning
                .message
                .contains("visual presentation was preserved")
        }));
    }

    #[test]
    fn changed_saved_payload_is_reported_and_updated_explicitly() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("Binary Decoder", egui::Pos2::ZERO)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[0].show_in_view = true;
        widget
            .graph_mut()
            .set_extension(
                PAYLOAD_SUBSCRIPTIONS_EXTENSION,
                SavedPayloadSubscriptions {
                    version: PAYLOAD_SUBSCRIPTIONS_VERSION,
                    subscriptions: vec![SavedPayloadSubscription {
                        target: SavedSubscriptionTarget::ShowInView {
                            node: source,
                            output: 0,
                        },
                        payload: "org.example.missing/v1".to_owned(),
                    }],
                },
            )
            .unwrap();

        let warnings =
            synchronize_payload_subscriptions(widget.graph_mut(), &BuilderRegistry::standard())
                .unwrap();

        assert!(warnings.iter().any(|warning| {
            warning
                .message
                .contains("now provides 'org.logicconduit.word/v1'")
        }));
    }

    #[test]
    fn unavailable_plugin_payload_identity_is_preserved_with_a_warning() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("Binary Decoder", egui::Pos2::ZERO)
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source).unwrap().outputs[0].show_in_view = true;
        widget
            .graph_mut()
            .set_extension(
                PAYLOAD_SUBSCRIPTIONS_EXTENSION,
                SavedPayloadSubscriptions {
                    version: PAYLOAD_SUBSCRIPTIONS_VERSION,
                    subscriptions: vec![SavedPayloadSubscription {
                        target: SavedSubscriptionTarget::ShowInView {
                            node: source,
                            output: 0,
                        },
                        payload: "org.example.unavailable/v1".to_owned(),
                    }],
                },
            )
            .unwrap();
        widget.graph_mut().nodes.get_mut(&source).unwrap().type_name =
            "Unavailable Plugin Decoder".to_owned();

        let warnings =
            synchronize_payload_subscriptions(widget.graph_mut(), &BuilderRegistry::standard())
                .unwrap();
        let saved: SavedPayloadSubscriptions = widget
            .graph()
            .extension(PAYLOAD_SUBSCRIPTIONS_EXTENSION)
            .unwrap()
            .unwrap();

        assert_eq!(saved.subscriptions[0].payload, "org.example.unavailable/v1");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.message.contains("payload is not registered"))
        );
    }

    #[test]
    fn explicit_viewer_input_round_trips_with_its_stable_payload_identity() {
        let mut widget = NodeGraphWidget::new(nodes::build_registry());
        let source = widget
            .add_node_at("Binary Decoder", egui::Pos2::ZERO)
            .unwrap();
        let viewer = widget
            .add_node_at("Viewer", egui::Pos2::new(200.0, 0.0))
            .unwrap();
        widget.graph_mut().add_connection(
            SocketId {
                node: source,
                index: 0,
                direction: SocketDirection::Output,
            },
            SocketId {
                node: viewer,
                index: 0,
                direction: SocketDirection::Input,
            },
        );
        let registry = BuilderRegistry::standard();
        synchronize_payload_subscriptions(widget.graph_mut(), &registry).unwrap();
        let json = serde_json::to_string(widget.graph()).unwrap();
        let mut restored: GraphState = serde_json::from_str(&json).unwrap();

        let warnings = synchronize_payload_subscriptions(&mut restored, &registry).unwrap();
        let saved: SavedPayloadSubscriptions = restored
            .extension(PAYLOAD_SUBSCRIPTIONS_EXTENSION)
            .unwrap()
            .unwrap();

        assert!(warnings.is_empty());
        assert_eq!(saved.subscriptions.len(), 1);
        assert_eq!(saved.subscriptions[0].payload, "org.logicconduit.word/v1");
        assert!(matches!(
            saved.subscriptions[0].target,
            SavedSubscriptionTarget::ViewerInput { node, input: 0 } if node == viewer
        ));
    }
}
