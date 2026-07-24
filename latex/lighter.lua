local lighter = {}
local lfs = require("lfs")
local md5 = require("md5")

local CACHE_VERSION = "2"

local function shell_quote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

function lighter.command_with_startup_options(command, theme, custom_theme, config)
  local arguments = { command }

  if theme ~= "" then
    arguments[#arguments + 1] = "--theme"
    arguments[#arguments + 1] = shell_quote(theme)
  end
  if custom_theme ~= "" then
    arguments[#arguments + 1] = "--custom-theme"
    arguments[#arguments + 1] = shell_quote(custom_theme)
  end
  if config ~= "" then
    arguments[#arguments + 1] = "--config"
    arguments[#arguments + 1] = shell_quote(config)
  end

  return table.concat(arguments, " ")
end

local function report_error(message)
  tex.error("lighter package error", { message })
end

local function read_file(path)
  local file, open_error = io.open(path, "rb")
  if not file then
    return nil, open_error
  end

  local contents = file:read("*a")
  file:close()
  return contents
end

local function output_file_path(path)
  if path:sub(1, 1) == "/" then
    return path
  end

  local output_directory = status.output_directory
  if output_directory and output_directory ~= "" then
    return output_directory .. "/" .. path
  end
  return path
end

function lighter.ensure_directory(path)
  path = output_file_path(path)
  local attributes = lfs.attributes(path)
  if attributes then
    if attributes.mode == "directory" then
      return true
    end
    report_error(
      "Could not create lighter cache directory " .. path .. ": path is not a directory"
    )
    return false
  end

  local ok, create_error = lfs.mkdir(path)
  if not ok then
    report_error("Could not create lighter cache directory " .. path .. ": " .. create_error)
    return false
  end
  return true
end

local function write_file(path, contents)
  local file, open_error = io.open(path, "wb")
  if not file then
    return nil, open_error
  end

  local ok, write_error = file:write(contents)
  file:close()
  return ok, write_error
end

local function file_exists(path)
  local file = io.open(path, "rb")
  if not file then
    return false
  end
  file:close()
  return true
end

local function cache_key(command, language, source, lines)
  -- Include every package input that can affect the rendered output. Bump the
  -- version whenever the daemon's rendering configuration changes.
  return md5.sumhexa(
    table.concat({ CACHE_VERSION, command, language, lines or "", source }, "\0")
  )
end

local function cache_path(output_path)
  return output_path .. ".lighter-cache"
end

local function cache_hit(output_path, key)
  if not file_exists(output_path) then
    return false
  end
  return read_file(cache_path(output_path)) == key
end

local function write_cached_output(output_path, key, contents)
  local key_path = cache_path(output_path)

  -- Never leave an old key next to partially updated output.
  os.remove(key_path)
  local _, write_error = write_file(output_path, contents)
  if write_error then
    report_error("Could not write highlighted listing " .. output_path .. ": " .. write_error)
    return false
  end

  local _, cache_error = write_file(key_path, key)
  if cache_error then
    report_error("Could not update lighter cache " .. key_path .. ": " .. cache_error)
    return false
  end
  return true
end

local function command_error(result_type, result_code, diagnostics)
  diagnostics = diagnostics and diagnostics:match("^%s*(.-)%s*$")
  if diagnostics and diagnostics ~= "" then
    return diagnostics
  end
  if result_type == "signal" then
    return "lighter was terminated by signal " .. tostring(result_code) .. "."
  end
  return "lighter exited with code " .. tostring(result_code or "unknown") .. "."
end

local function run(command, language, source, lines)
  local input_path = os.tmpname()
  local output_path = os.tmpname()
  local diagnostics_path = os.tmpname()

  local _, input_error = write_file(input_path, source)
  if input_error then
    os.remove(input_path)
    os.remove(output_path)
    os.remove(diagnostics_path)
    return nil, "Could not prepare lighter input: " .. input_error
  end

  local lines_option = lines and " --lines " .. shell_quote(lines) or ""
  local invocation = string.format(
    "%s --lang %s%s < %s > %s 2> %s",
    command,
    shell_quote(language),
    lines_option,
    shell_quote(input_path),
    shell_quote(output_path),
    shell_quote(diagnostics_path)
  )
  local ok, result_type, result_code = os.execute(invocation)
  local output, output_error = read_file(output_path)
  local diagnostics = read_file(diagnostics_path)

  os.remove(input_path)
  os.remove(output_path)
  os.remove(diagnostics_path)

  -- Lua 5.1 returns 0 on success; newer Lua versions return true, "exit", 0.
  if ok ~= true and ok ~= 0 then
    return nil, command_error(result_type, result_code, diagnostics)
  end
  if output == nil then
    return nil, "Could not read lighter output: " .. tostring(output_error)
  end
  return output
end

local function select_lines(source, lines)
  if not lines then
    return source
  end

  local start_text, end_text = lines:match("^(%d*):(%d*)$")
  local start_line = tonumber(start_text) or 1
  local end_line = tonumber(end_text)
  if not start_text or (start_text == "" and end_text == "")
      or start_line < 1 or (end_line and (end_line < 1 or end_line < start_line)) then
    return nil
  end

  local function line_start(line)
    local offset = 1
    for _ = 2, line do
      local newline = source:find("\n", offset, true)
      if not newline then
        return #source + 1
      end
      offset = newline + 1
    end
    return offset
  end

  local first = line_start(start_line)
  local last = end_line and line_start(end_line + 1) - 1 or #source
  return source:sub(first, last)
end

local function render_text(source)
  source = source:gsub("\r\n", "\n"):gsub("\n+$", "")
  local output = {}

  for index = 1, #source do
    local character = source:sub(index, index)
    if character == "\\" or character == "{" or character == "}" then
      output[#output + 1] = "\\" .. character
    else
      output[#output + 1] = character
    end
  end
  return table.concat(output)
end

function lighter.highlight(command, language, source_path, output_path, lines)
  if lines == "" then
    lines = nil
  end
  output_path = output_file_path(output_path)
  local source, source_error = read_file(source_path)
  if not source then
    source, source_error = read_file(output_file_path(source_path))
  end
  if not source then
    report_error("Could not read listing source " .. source_path .. ": " .. source_error)
    os.remove(cache_path(output_path))
    write_file(output_path, "")
    return
  end

  local key = cache_key(command, language, source, lines)
  if cache_hit(output_path, key) then
    return
  end

  -- Invalidate before doing any work so a lighter failure cannot make stale
  -- output appear valid on a later run.
  os.remove(cache_path(output_path))

  -- Plain text has no syntax to highlight, so emit it literally.
  if language == "text" then
    source = select_lines(source, lines)
    if not source then
      report_error("Invalid line range " .. lines ..
        ": expected start:end, :end, or start: with one-based line numbers.")
      write_file(output_path, "")
      return
    end
    write_cached_output(output_path, key, render_text(source))
    return
  end

  local response, response_error = run(command, language, source, lines)
  if response == nil then
    report_error(response_error)
    write_file(output_path, "")
    return
  end
  write_cached_output(output_path, key, response)
end

function lighter.remove(path)
  os.remove(path)
  os.remove(output_file_path(path))
end

return lighter
