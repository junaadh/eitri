-- A small module in the shape of a setup script: definitions, then a few
-- top-level computations and one result for the host.
local function clamp(value, low, high)
    if value < low then
        return low
    elseif value > high then
        return high
    end
    return value
end
local function scale(value, percent)
    return value * percent // 100
end
local function enabled(level, threshold)
    return level >= threshold and level ~= 0
end
local function pick(flag, yes, no)
    if flag then return yes else return no end
end
local width = 120
local height = 40
local gutter = clamp(width // 30, 2, 6)
local columns = width - gutter * 2
local rows = height - 2
local tab_width = 4
local scroll_off = clamp(rows // 5, 3, 10)
local font_size = scale(14, 110)
local line_height = scale(font_size, 130)
local level = 3
local verbose = enabled(level, 2)
local debug = enabled(level, 5)
local timeout = pick(verbose, 500, 1000)
local retries = pick(debug, 10, 3)
local checksum = 0
checksum = checksum + width + height
checksum = checksum + gutter * 3 + columns
checksum = checksum + rows * 7 - tab_width
checksum = checksum + scroll_off + font_size + line_height
checksum = checksum + timeout // retries
checksum = checksum + pick(verbose and not debug, 1, 0)
emit(checksum)
