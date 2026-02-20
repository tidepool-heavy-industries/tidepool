/// Internal type system for guided generation. NEVER exposed publicly.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SimpleType {
    Int,
    Bool,
    Char,
    Fun(Box<SimpleType>, Box<SimpleType>),
    Maybe(Box<SimpleType>),
    Pair(Box<SimpleType>, Box<SimpleType>),
}
