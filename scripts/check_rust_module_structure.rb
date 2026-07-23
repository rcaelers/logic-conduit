#!/usr/bin/env ruby
# frozen_string_literal: true

# Enforces docs/RESPONSIBILITY_AND_VISIBILITY_DESIGN.md. This deliberately
# checks source structure rather than formatting; rustc remains authoritative
# for name resolution and `unreachable_pub`.

ROOT = File.expand_path("..", __dir__)
SOURCE_GLOBS = ["crates/**/*.rs", "plugins/**/*.rs"].freeze
ROOT_FILES = %w[lib.rs main.rs mod.rs].freeze

PUBLIC_MODULES = {
  "crates/signal_processing/src/lib.rs" => %w[capture derived_word_store live_capture live_capture_store waveform_index],
  "crates/logic_analyzer_processing/src/lib.rs" => %w[nodes support test_support types],
  "crates/logic_analyzer_processing/src/support/mod.rs" => %w[logic_analyzer],
  "crates/logic_analyzer_processing/src/nodes/mod.rs" => %w[decoders logic sinks sources],
  "crates/logic_analyzer_processing/src/nodes/decoders/mod.rs" => %w[parallel_decoder spi_decoder uart_decoder],
  "crates/logic_analyzer_processing/src/nodes/logic/mod.rs" => %w[
    buffer logic_gate sr_latch text_formatter trigger_counter word_matcher
  ],
  "crates/logic_analyzer_processing/src/nodes/sinks/mod.rs" => %w[
    binary_file_writer csv_word_writer discard_writer text_file_writer tgck_recorder
  ],
  "crates/logic_analyzer_processing/src/nodes/sources/mod.rs" => %w[
    dsl_file dslogic_u3pro16 sigrok_file synthetic_capture_source synthetic_uart_source
  ],
  "crates/logic_analyzer_graph_api/src/lib.rs" => %w[node node_support],
  "crates/logic_analyzer_graph/src/lib.rs" => %w[host node node_support test_support],
  "crates/logic_analyzer_graph_nodes/src/lib.rs" => %w[test_support]
}.freeze

errors = []

def relative(path)
  path.delete_prefix("#{ROOT}/")
end

def line_number(source, offset)
  source[0...offset].count("\n") + 1
end

def implementation_source(source)
  test_module = source.index(/^\s*#\s*\[\s*cfg\s*\([^\]]*\btest\b[^\]]*\)\s*\]\s*\n\s*mod\s+\w*tests\b/)
  test_module.nil? ? source : source[0...test_module]
end

files = SOURCE_GLOBS.flat_map { |glob| Dir.glob(File.join(ROOT, glob)) }.sort

ui_compiler_free_functions = %w[
  apply_live_capture_edit
  derived_cache_configs_by_node
  discover_capture_presentation
  discover_live_capture_feature
  discover_trigger_configuration
  lower
  sampling_overlay_candidates
  start_app_run
  start_app_run_with_source_overrides
  start_live_analysis
  synchronize_payload_subscriptions
].freeze

graph_api_manifest = File.read(File.join(ROOT, "crates/logic_analyzer_graph_api/Cargo.toml"))
%w[logic-analyzer-graph logic-analyzer-processing logic-analyzer-ui].each do |dependency|
  if graph_api_manifest.match?(/^#{Regexp.escape(dependency)}\s*=/)
    errors << "crates/logic_analyzer_graph_api/Cargo.toml: graph API must not depend on #{dependency}"
  end
end

graph_manifest = File.read(File.join(ROOT, "crates/logic_analyzer_graph/Cargo.toml"))
graph_production_manifest = graph_manifest.split(/^\[dev-dependencies\]\s*$/, 2).first
if graph_production_manifest.match?(/^logic-analyzer-graph-nodes\s*=/)
  errors << "crates/logic_analyzer_graph/Cargo.toml: compiler production code must not depend on built-in graph nodes"
end

graph_nodes_manifest = File.read(File.join(ROOT, "crates/logic_analyzer_graph_nodes/Cargo.toml"))
graph_nodes_production_manifest = graph_nodes_manifest.split(/^\[dev-dependencies\]\s*$/, 2).first
if graph_nodes_production_manifest.match?(/^logic-analyzer-graph\s*=/)
  errors << "crates/logic_analyzer_graph_nodes/Cargo.toml: built-in nodes submit graph API contracts and must not depend on the compiler"
end

files.each do |path|
  rel = relative(path)
  source = File.read(path)

  if rel.start_with?("crates/logic_analyzer_ui/src/")
    implementation = implementation_source(source)
    implementation.to_enum(:scan, /\bBuilderRegistry\b/).each do
      errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: UI hosts use GraphCompiler, not BuilderRegistry"
    end
    ui_compiler_free_functions.each do |function|
      implementation.to_enum(:scan, /\bcompiler::#{Regexp.escape(function)}\s*\(/).each do
        errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: UI hosts call GraphCompiler##{function}"
      end
    end
  end

  graph_node_implementation = rel.start_with?("crates/logic_analyzer_graph_nodes/src/nodes/")
  plugin_implementation = rel.start_with?("plugins/")
  if graph_node_implementation || plugin_implementation
    implementation = implementation_source(source)
    implementation.to_enum(:scan, /\bCompileCtx\b/).each do
      errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: graph-node implementations receive NodeBuildContext, not host CompileCtx"
    end
  end

  source.to_enum(:scan, /\bpub\s*\((?:super|in\s+[^)]*)\)/).each do
    errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: pub(super) and pub(in ...) are forbidden"
  end

  declaration = /^\s*(?<visibility>pub(?:\([^)]*\))?\s+)?mod\s+(?<name>[A-Za-z_][A-Za-z0-9_]*)\s*(?:;|\{)/
  source.to_enum(:scan, declaration).each do
    match = Regexp.last_match
    name = match[:name]
    line = line_number(source, match.begin(0))

    unless ROOT_FILES.include?(File.basename(path))
      preceding = source[[match.begin(0) - 200, 0].max...match.begin(0)]
      test_module = name.include?("tests") && preceding.match?(/#\s*\[\s*cfg\s*\([^\]]*\btest\b/)
      unless test_module
        errors << "#{rel}:#{line}: module declarations belong only in lib.rs, main.rs, or mod.rs"
      end
    end

    next unless match[:visibility]&.strip == "pub"

    allowed = PUBLIC_MODULES.fetch(rel, [])
    unless allowed.include?(name)
      errors << "#{rel}:#{line}: public module #{name.inspect} is not in the allowlist"
    end

    module_directory = File.join(File.dirname(path), name, "mod.rs")
    unless File.file?(module_directory)
      errors << "#{rel}:#{line}: public module #{name.inspect} must be directory-backed by #{relative(module_directory)}"
    end
  end

  next unless File.basename(path) == "mod.rs"

  implementation = /^\s*(?:pub(?:\([^)]*\))?\s+)?(?:async\s+|unsafe\s+)?(?:struct|enum|union|trait|fn|const|static|type)\b|^\s*impl(?:\s|<)|^\s*macro_rules!/
  source.to_enum(:scan, implementation).each do
    errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: mod.rs files may contain declarations and re-exports only"
  end
  source.to_enum(:scan, /\b(?:cfg_select|include)!\s*[({]/).each do
    errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: executable selection/include macros are not allowed in mod.rs"
  end
  source.to_enum(:scan, /^\s*use\s+/).each do
    errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: mod.rs imports must be facade re-exports"
  end

  concrete_graph_node = rel.match?(%r{\Acrates/logic_analyzer_graph_nodes/src/nodes/(?:decoders|logic|sinks|sources)/[^/]+/mod\.rs\z})
  if concrete_graph_node
    source.to_enum(:scan, /^\s*pub(?:\(crate\))?\s+use\s+/).each do
      errors << "#{rel}:#{line_number(source, Regexp.last_match.begin(0))}: concrete graph nodes must not re-export symbols"
    end
  end
end

# Named record structs use one field visibility. This intentionally ignores
# tuple structs: their fields are positional construction APIs and rustc's
# visibility checks already cover each position.
files.each do |path|
  rel = relative(path)
  source = File.read(path)
  source.to_enum(:scan, /\bstruct\s+([A-Za-z_][A-Za-z0-9_]*)/).each do
    match = Regexp.last_match
    name = match[1]
    opening = source.index("{", match.end(0))
    terminator = source.index(";", match.end(0))
    next if opening.nil? || (!terminator.nil? && terminator < opening)

    depth = 1
    body_length = nil
    source[(opening + 1)..].each_char.with_index do |character, index|
      case character
      when "{" then depth += 1
      when "}" then depth -= 1
      end
      if depth.zero?
        body_length = index
        break
      end
    end
    next if body_length.nil?

    body = source[(opening + 1), body_length]
    field_depth = 0
    visibilities = []
    body.each_line do |line|
      if field_depth.zero? && (field = line.match(/^\s*(?:(pub(?:\(crate\))?)\s+)?[A-Za-z_][A-Za-z0-9_]*\s*:/))
        visibilities << (field[1] || "private")
      end
      field_depth += line.count("{") - line.count("}")
    end
    kinds = visibilities.uniq
    next unless kinds.length > 1

    errors << "#{rel}:#{line_number(source, match.begin(0))}: struct #{name} mixes field visibility (#{kinds.join(", ")})"
  end
end

if errors.empty?
  puts "Rust module structure matches the responsibility and visibility design."
  exit 0
end

warn errors.join("\n")
warn "#{errors.length} Rust module-structure violation#{errors.length == 1 ? "" : "s"} found."
exit 1
