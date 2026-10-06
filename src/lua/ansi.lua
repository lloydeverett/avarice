-- Original to avarice. This file is not taken from Astra, and is not derived from anything.
--
-- This is a CORE module: the core registers it in every runtime, whatever the profile, and no
-- build can leave it out, because `print` uses these codes to highlight what it prints (ADR 0011).
-- It is also PURE: this file is all of it. There is no Rust behind it, so it computes over the
-- values it is given and reaches nothing outside the Lua state, and every limit a runtime puts on
-- Lua (the memory cap, the time limit) applies to it. That is why it is safe to have in a sandbox,
-- and giving it a Rust half would need that decision made again (ADR 0007).
--
-- Every value is a complete escape sequence, meant to be concatenated with text:
--
--     ansi.bold .. ansi.fg.red .. "error" .. ansi.reset
--
-- Nothing here looks at whether the output is a terminal or whether NO_COLOR is set. That is for
-- the program, or the host, to decide.
--
-- The runtime evaluates this file once for `print`, which keeps the codes it needs in locals, and
-- again if a script requires it, so a script that edits the table it required cannot change how
-- `print` highlights.

---@meta

---@alias ansi_code string A complete escape sequence, such as "\27[1m".

---@class AnsiColors
---@field black ansi_code
---@field red ansi_code
---@field green ansi_code
---@field yellow ansi_code
---@field blue ansi_code
---@field magenta ansi_code
---@field cyan ansi_code
---@field white ansi_code
---@field bright_black ansi_code
---@field bright_red ansi_code
---@field bright_green ansi_code
---@field bright_yellow ansi_code
---@field bright_blue ansi_code
---@field bright_magenta ansi_code
---@field bright_cyan ansi_code
---@field bright_white ansi_code
---The terminal's own colour.
---@field default ansi_code
---True colour. Each component is an integer 0-255; anything else raises.
---@field rgb fun(red: integer, green: integer, blue: integer): ansi_code
---One of the 256 palette colours. `n` is an integer 0-255; anything else raises.
---@field color256 fun(n: integer): ansi_code
---True colour from six hex digits, with or without a leading "#", in either case. The
---three-digit form is not accepted. Anything else raises.
---@field hex fun(s: string): ansi_code

---@class Ansi
---@field reset ansi_code Turns off every style and colour.
---@field bold ansi_code
---@field dim ansi_code
---@field italic ansi_code
---@field underline ansi_code
---@field blink ansi_code
---@field reverse ansi_code
---@field hidden ansi_code
---@field strikethrough ansi_code
---Bold and dim are both switched off by the one code the terminal has for it, so `no_bold` and
---`no_dim` are the same string.
---@field no_bold ansi_code
---@field no_dim ansi_code
---@field no_italic ansi_code
---@field no_underline ansi_code
---@field no_blink ansi_code
---@field no_reverse ansi_code
---@field no_hidden ansi_code
---@field no_strikethrough ansi_code
---@field fg AnsiColors Foreground colours.
---@field bg AnsiColors Background colours.

local ESC = "\27["

---The escape sequence for a list of SGR parameters.
---@param ... integer
---@return ansi_code
local function sgr(...)
    return ESC .. table.concat({ ... }, ";") .. "m"
end

---`value` as an integer 0-255, or an error that names it, blamed on whoever called the function
---that called this one (level 3: this, that function, its caller).
---@param value any
---@param what string
---@return integer
local function byte(value, what)
    if type(value) == "number" then
        -- A float with an integer value, such as 255.0, is accepted, as it is wherever Lua takes
        -- an integer. A string is not, though Lua would convert one, because nobody means it.
        local n = math.tointeger(value)
        if n and n >= 0 and n <= 255 then
            return n
        end
    end
    error(what .. " must be an integer 0–255", 3)
end

local names = { "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white" }

---The colours for one of foreground and background.
---@param normal integer First of the eight normal colours: 30 or 40.
---@param bright integer First of the eight bright colours: 90 or 100.
---@param extended integer The code that introduces a true colour or palette colour: 38 or 48.
---@param default integer The code for the terminal's own colour: 39 or 49.
---@return AnsiColors
local function colors(normal, bright, extended, default)
    local set = { default = sgr(default) }
    for i, name in ipairs(names) do
        set[name] = sgr(normal + i - 1)
        set["bright_" .. name] = sgr(bright + i - 1)
    end

    function set.rgb(red, green, blue)
        local r = byte(red, "rgb: red component")
        local g = byte(green, "rgb: green component")
        local b = byte(blue, "rgb: blue component")
        return sgr(extended, 2, r, g, b)
    end

    function set.color256(n)
        local index = byte(n, "color256: n")
        return sgr(extended, 5, index)
    end

    function set.hex(s)
        local digits = type(s) == "string" and s:match("^#?(%x%x%x%x%x%x)$")
        if not digits then
            error("hex: expected six hex digits, optionally after a #", 2)
        end
        return set.rgb(
            tonumber(digits:sub(1, 2), 16),
            tonumber(digits:sub(3, 4), 16),
            tonumber(digits:sub(5, 6), 16)
        )
    end

    return set
end

---@type Ansi
return {
    reset = sgr(0),

    bold = sgr(1),
    dim = sgr(2),
    italic = sgr(3),
    underline = sgr(4),
    blink = sgr(5),
    reverse = sgr(7),
    hidden = sgr(8),
    strikethrough = sgr(9),

    -- Bold and dim are switched off together: the terminal has one code for both.
    no_bold = sgr(22),
    no_dim = sgr(22),
    no_italic = sgr(23),
    no_underline = sgr(24),
    no_blink = sgr(25),
    no_reverse = sgr(27),
    no_hidden = sgr(28),
    no_strikethrough = sgr(29),

    fg = colors(30, 90, 38, 39),
    bg = colors(40, 100, 48, 49),
}
