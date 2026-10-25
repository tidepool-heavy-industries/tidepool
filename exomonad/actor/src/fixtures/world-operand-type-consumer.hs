operandOnly <- if input == object [("greeting", String "operand only")] then pure input else error "operand-local TYPE support changed the input"
