local total = 0
local i = 0
while i < 1000 do
    total = total + id(i)
    i = i + 1
end
emit(total)
