import qualified Persona.Memory as Memory
beforeMemory <- Memory.recall Memory.Human
display beforeMemory
memoryEdit <- Memory.revise Memory.Human (<> "\nAn experiment worth returning to.\n")
display memoryEdit
