# Design sources

The seed text is newly authored. Its small, evolving persona and human memory
blocks follow the pattern discussed in Letta's persona/human templates:
https://github.com/letta-ai/letta-code/tree/main/src/agent/prompts
No Letta source or prompt text is copied into this seed.

Tavily's search and extract contracts:
https://docs.tavily.com/documentation/api-reference/endpoint/search
https://docs.tavily.com/documentation/api-reference/endpoint/extract
The adapter requests basic retrieval and no generated answer. Semantic filtering
is authored with the existing pinned Jev DSL, after retrieval.
