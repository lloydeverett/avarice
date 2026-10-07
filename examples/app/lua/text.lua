-- A module the program keeps beside itself, found in the module store.
local text = {}

function text.shout(s)
  return s:upper()
end

return text
