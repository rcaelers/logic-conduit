#!/usr/bin/env ruby
# frozen_string_literal: true

# Sort Rust imports into four groups:
#   1. language crates (std/core/alloc)
#   2. third-party crates
#   3. crates from this Cargo workspace
#   4. the current crate (crate/self/super)

require "json"
require "open3"

ROOT = File.expand_path("..", __dir__)

def run!(*command)
  success = system(*command, chdir: ROOT)
  abort("command failed: #{command.join(' ')}") unless success
end

metadata_json, status = Open3.capture2(
  "cargo",
  "metadata",
  "--format-version=1",
  "--no-deps",
  chdir: ROOT
)
abort("cargo metadata failed") unless status.success?

workspace_crates = JSON.parse(metadata_json)
  .fetch("packages")
  .map { |package| package.fetch("name").tr("-", "_") }
  .to_h { |name| [name, true] }

# Let rustfmt parse and sort every import first. Preserve mode in rustfmt.toml
# keeps the additional workspace split made below on subsequent format runs.
run!("cargo", "fmt", "--all", "--", "--config", "group_imports=StdExternalCrate")

rust_files, status = Open3.capture2("rg", "--files", "--glob", "*.rs", chdir: ROOT)
abort("rg failed while listing Rust files") unless status.success?

def import_entries(paragraph)
  entries = []
  pending = []
  current = nil

  paragraph.each do |line|
    if current
      current << line
      if line.rstrip.end_with?(";")
        entries << current
        current = nil
      end
    elsif line.match?(/^\s*#\[/)
      pending << line
    elsif line.match?(/^\s*use\s+/)
      current = pending + [line]
      pending = []
      if line.rstrip.end_with?(";")
        entries << current
        current = nil
      end
    else
      return nil
    end
  end

  return nil if current || !pending.empty?

  entries
end

def import_root(entry)
  use_line = entry.find { |line| line.match?(/^\s*use\s+/) }
  use_line&.match(/^\s*use\s+(?:::)?([A-Za-z_][A-Za-z0-9_]*)/)&.captures&.first
end

def category(root, workspace_crates, local_modules)
  return 0 if %w[std core alloc].include?(root)
  return 3 if %w[crate self super].include?(root) || local_modules.key?(root)
  return 2 if workspace_crates.key?(root)

  1
end

rust_files.lines(chomp: true).each do |relative_path|
  path = File.join(ROOT, relative_path)
  lines = File.readlines(path, mode: "r:BOM|UTF-8")
  local_modules = lines
    .map { |line| line[/^\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)/, 1] }
    .compact
    .to_h { |name| [name, true] }
  output = []
  paragraph = []

  flush = lambda do
    unless paragraph.empty?
      entries = import_entries(paragraph)
      if entries
        grouped = entries.group_by do |entry|
          category(import_root(entry), workspace_crates, local_modules)
        end
        output.concat(
          grouped.keys.sort.flat_map.with_index do |group, index|
            separator = index.zero? ? [] : ["\n"]
            separator + grouped.fetch(group).flatten(1)
          end
        )
      else
        output.concat(paragraph)
      end
      paragraph.clear
    end
  end

  lines.each do |line|
    if line.strip.empty?
      flush.call
      output << line
    else
      paragraph << line
    end
  end
  flush.call

  # Group splitting can leave more than one blank separator because rustfmt
  # had already separated a subset. Keep source spacing stable elsewhere.
  normalized = output.join.gsub(/\n{3,}(?=\s*(?:#\[[^\n]+\]\n\s*)?use\s)/, "\n\n")
  File.write(path, normalized) unless normalized == lines.join
end

run!("cargo", "fmt", "--all")
