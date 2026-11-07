local function classify(n)
    if n % 15 == 0 then
        return 4
    elseif n % 5 == 0 then
        return 3
    elseif n % 3 == 0 then
        return 2
    end
    return 1
end
local score = 0
local i = 0
while i < 1000 do
    local kind = classify(i)
    if kind > 2 and i % 2 == 0 or kind == 1 and i > 500 then
        score = score + kind
    elseif not (kind == 2) then
        score = score - 1
    else
        if i < 100 then score = score + 7 else score = score + 5 end
    end
    i = i + 1
end
emit(score)
