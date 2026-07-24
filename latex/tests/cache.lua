local lighter = dofile("lighter/lighter.lua")

local function write_file(path, contents)
  local file = assert(io.open(path, "wb"))
  assert(file:write(contents))
  file:close()
end

local function read_file(path)
  local file = assert(io.open(path, "rb"))
  local contents = file:read("*a")
  file:close()
  return contents
end

local function file_exists(path)
  local file = io.open(path, "rb")
  if not file then
    return false
  end
  file:close()
  return true
end

local base = os.tmpname()
os.remove(base)
local source_path = base .. ".source"
local output_path = base .. ".tex"
local cache_path = output_path .. ".lighter-cache"
local mock_path = base .. ".sh"

assert(lighter.command_with_startup_options("lighter", "", "", "") == "lighter")
assert(
  lighter.command_with_startup_options(
    "lighter", "Catppuccin Latte", "", "config file.toml"
  ) == "lighter --theme 'Catppuccin Latte' --config 'config file.toml'"
)
assert(
  lighter.command_with_startup_options(
    "lighter", "", "author's theme.toml", ""
  ) == "lighter --custom-theme 'author'\\''s theme.toml'"
)

write_file(source_path, "alpha {one}\n")
lighter.highlight("command-a", "text", source_path, output_path)
assert(read_file(output_path) == "alpha \\{one\\}")
local first_key = read_file(cache_path)

-- Matching source and rendering inputs must reuse the existing output.
write_file(output_path, "cache hit")
lighter.highlight("command-a", "text", source_path, output_path)
assert(read_file(output_path) == "cache hit")
assert(read_file(cache_path) == first_key)

-- A source change must invalidate and replace the cached output.
write_file(source_path, "beta\n")
lighter.highlight("command-a", "text", source_path, output_path)
assert(read_file(output_path) == "beta")
assert(read_file(cache_path) ~= first_key)

-- Rendering configuration is part of the key as well.
write_file(output_path, "stale")
lighter.highlight("command-b", "text", source_path, output_path)
assert(read_file(output_path) == "beta")

-- Inclusive and open-ended line ranges select plain-text source lines.
write_file(source_path, "one\ntwo\nthree\nfour\n")
lighter.highlight("command-a", "text", source_path, output_path, "2:3")
assert(read_file(output_path) == "two\nthree")
lighter.highlight("command-a", "text", source_path, output_path, ":2")
assert(read_file(output_path) == "one\ntwo")
lighter.highlight("command-a", "text", source_path, output_path, "3:")
assert(read_file(output_path) == "three\nfour")

-- Non-text sources are sent to `lighter --lang <language>` through stdin.
write_file(mock_path, [=[
if [ "$1" != "--theme" ] || [ "$2" != "Catppuccin Latte" ] ||
    [ "$3" != "--config" ] || [ "$4" != "config file.toml" ] ||
    [ "$5" != "--lang" ]; then
  echo "missing startup, theme, config, or language options" >&2
  exit 2
fi
if [ "$6" = "failure" ]; then
  echo "deliberate lighter failure" >&2
  exit 7
fi
printf 'highlighted:'
cat
]=])
write_file(source_path, "def answer():\n    return 42\n")
local mock_command = lighter.command_with_startup_options(
  "sh " .. mock_path, "Catppuccin Latte", "", "config file.toml"
)
lighter.highlight(mock_command, "python", source_path, output_path)
assert(read_file(output_path) == "highlighted:def answer():\n    return 42\n")
assert(file_exists(cache_path))

-- Non-text ranges are forwarded to the lighter command and affect the cache.
write_file(mock_path, [=[
if [ "$6" = "failure" ]; then
  echo "deliberate lighter failure" >&2
  exit 7
fi
if [ "$1" != "--theme" ] || [ "$3" != "--config" ] ||
    [ "$5" != "--lang" ] || [ "$7" != "--lines" ]; then
  echo "missing startup, language, or line options" >&2
  exit 2
fi
printf 'lines=%s:' "$8"
cat
]=])
lighter.highlight(mock_command, "python", source_path, output_path, "2:")
assert(read_file(output_path) == "lines=2::def answer():\n    return 42\n")

-- Failed calls surface stderr, clear stale output, and remain uncached.
local original_tex_error = tex.error
local reported_error
tex.error = function(_, messages)
  reported_error = messages[1]
end
lighter.highlight(mock_command, "failure", source_path, output_path)
tex.error = original_tex_error
assert(reported_error == "deliberate lighter failure")
assert(read_file(output_path) == "")
assert(not file_exists(cache_path))

os.remove(source_path)
os.remove(output_path)
os.remove(cache_path)
os.remove(mock_path)
