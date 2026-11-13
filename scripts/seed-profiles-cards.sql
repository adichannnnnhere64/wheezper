-- Seed: the default profile, a second profile, and shared answer cards.
-- Idempotent: safe to run twice.
--
-- The profile `summary` is the ONLY source of personal claims the assistant may
-- use. Everything else in an answer has to be technical knowledge or a card.

INSERT INTO profiles (id, slug, display_name, summary, default_model)
VALUES
  ('11111111-1111-4111-8111-111111111111', 'default', 'Adrian Radores',
   'Adrian Radores, Philippines. Back-end-heavy full-stack developer since 2019.
- U+ Realty (2019-2022): Java Spring Boot services in production, PostgreSQL, Spring Boot Actuator, Docker, GitHub Actions, Terraform on Azure.
- Chanz IT (2022-2024): Laravel multi-tenant e-commerce, WebSockets, CI/CD, PHPUnit/Pest at 80%+ coverage.
- RePay.ph (2025-present): rewrote the Laravel backend as a Java Spring Boot service; schema design, REST APIs, integrations, tests.
- Languages: PHP, Java, JavaScript/TypeScript, SQL. Frameworks: Laravel, Spring Boot, Vue, React.
- Builds local RAG (embeddings + retrieval) and uses Claude/Llama as coding assistants.
- Honest gaps: no production experience with Gemini Enterprise, Vertex AI, CrewAI, or model training.',
   '')
ON CONFLICT (slug) DO NOTHING;

INSERT INTO answer_cards (id, topic, question, answer, keywords, profile_id) VALUES
 ('aaaaaaa1-0000-4000-8000-000000000001', 'agent-basics',
  'What are the components of an agent?',
  'An agent is a model in a loop with tools, state and guardrails. 1) Model + instructions that define the role and stop conditions. 2) Tools: typed functions with schemas and descriptions. 3) Memory/state: history, session scratchpad, persisted context. 4) Orchestration loop: plan, call tool, observe, decide, with a max-step stop. 5) Runtime: auth, secrets, timeouts, retries, rate limits. 6) Guardrails: input/output validation, tool allow-lists, budgets. 7) Observability: traces of tool calls, latency, cost, replay.',
  'agent components tools memory orchestration runtime guardrails observability what is an agent made of', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000002', 'agent-basics',
  'Multi-agent architectures and sequencing',
  'Four patterns: sequential pipeline; supervisor/orchestrator routing to specialist workers; hierarchical; shared-state blackboard. Specialisation buys accuracy and clearer permissions, and costs latency, tokens and a harder debugging story. Start with one agent and split only when a sub-task needs different tools, permissions or a prompt that keeps confusing the main one.',
  'multi agent architecture orchestration supervisor router sequence handoff', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000003', 'integration',
  'How does an agent connect to and consume enterprise data like SAP?',
  'Three layers. Transport: REST/OData or API gateway, SOAP/RFC via middleware, read replica, nightly extract, or a managed connector (SAP connector, MCP server) - the agent calls a tool, never the source. Auth/network: OAuth2 client credentials or service account, mTLS or private IP/VPN, least privilege, read-only for the agent. What the agent receives: a typed tool interface with pagination, retries, timeouts and a normalised response; live transactional reads for single records, prepared extracts or a vector/graph index for search; cache safely and redact PII before the model sees it.',
  'sap enterprise data integration connector api gateway oauth odata rfc bapi bigquery extract', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000004', 'integration',
  'Knowledge graph versus vector index',
  'Vector search answers fuzzy semantic questions over unstructured text. A graph answers relational questions - which contracts affect this customer entitlement - where the answer is a path, not a passage. Use the graph when the question is about relationships and joins; use vectors when it is about meaning inside documents.',
  'knowledge graph vector index embeddings retrieval relational', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000005', 'evaluation',
  'What is LLM-as-a-judge and how do we evaluate agents?',
  'A second model grades the agent output against a golden set and an explicit rubric, returning structured JSON with score and reason. Design: judge on a different model or at least a different prompt, temperature 0, criteria-based rubric, and calibration - hand-label a sample and measure judge-vs-human agreement before trusting it in CI. Watch judge bias: prefers longer answers, prefers the first option in pairwise. Beyond the judge: task success, tool-call correctness, groundedness, refusal correctness, cost and latency.',
  'llm as a judge evaluation golden set rubric groundedness automated testing quality', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000006', 'evaluation',
  'How do you automate agent testing in a pipeline?',
  'Version a golden set with the code, run the agent against it in CI, score with the judge, and fail the build when the score drops. Add deterministic assertions for tool arguments and schema validity so you do not rely on the judge for what code can check. Keep per-case traces so a regression is one diff away.',
  'automated testing pipeline ci agent evaluation regression golden dataset', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000007', 'safety',
  'Guardrails for agents',
  'Input validation and PII redaction before the model; tool allow-lists with typed schemas; max steps and spend budgets; output schema validation; content filters; and human approval for irreversible actions. Guardrails are code paths, not prompt sentences.',
  'guardrails safety pii redaction allow list budget schema validation content filter', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000008', 'safety',
  'What is human-in-the-loop and when is it used?',
  'A designed pause for a person to approve, reject or supply something the agent cannot get. It belongs before irreversible or costly actions, when required information is missing, when confidence is low, and as sampled QA on autonomous runs. Implementation: persist workflow state so the run resumes after approval, a timeout default that fails closed, and an audit trail of who approved what and when.',
  'human in the loop approval review escalation audit timeout resume workflow', NULL),

 ('aaaaaaa1-0000-4000-8000-000000000009', 'platform',
  'Gemini Enterprise / Vertex AI experience',
  'I have not built on Gemini Enterprise or Vertex AI in production. What I have done is the same shape on other stacks: Python agent loops with tool calling, a local RAG service with embeddings and retrieval, and back-end services with Spring Boot, PostgreSQL and Docker. On a platform I have not used, I read the solution design document, start with the thinnest vertical slice - one tool, one golden case - and grow it.',
  'gemini enterprise vertex ai gcp google cloud', NULL),

 ('aaaaaaa1-0000-4000-8000-00000000000a', 'platform',
  'Python experience from a Java background',
  'I write Python for tooling and data work - pandas, scikit-learn for a credit-default model with honest evaluation, FastAPI to serve it, pytest for the contract tests. My production back-end is Java Spring Boot, so I bring testing, CI and deployment discipline to the Python side rather than treating it as scripts.',
  'python java spring boot tooling fastapi pytest scikit-learn pandas credit default', NULL)
ON CONFLICT (id) DO NOTHING;
