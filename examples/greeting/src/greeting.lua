-- The Lua half of `greeting`, given the table its Rust half built.
local greeting = ...

-- Greets each of `names`, one to a line.
function greeting.hello_all(names)
  local lines = {}
  for i, name in ipairs(names) do
    lines[i] = greeting.hello(name)
  end
  return table.concat(lines, "\n")
end

return greeting
