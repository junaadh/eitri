local function mix(n)
    local total = 0
    local i = 0
    while i < n do
        total = total + i * 3 - i // 7 + i % 5
        total = total * 31 + i
        i = i + 1
    end
    return total
end
emit(mix(1000))
