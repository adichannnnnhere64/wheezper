-- Five more cards for questions that are answerable from the profile and were
-- being refused ("what databases have you used in production?"). Idempotent.
INSERT INTO answer_cards (id, topic, question, answer, keywords, profile_id) VALUES
 ('aaaaaaa1-0000-4000-8000-00000000000b', 'background',
  'What databases have you used in production?',
  'PostgreSQL is my default - schema design, migrations, indexing and query plans, in Java Spring Boot services and in Laravel. MySQL where the platform required it. I keep transactions small, avoid N+1 queries, and reach for an index or a materialised view before denormalising.',
  'databases database postgres postgresql mysql sql schema indexing storage', NULL),

 ('aaaaaaa1-0000-4000-8000-00000000000c', 'background',
  'What do you do in your current role?',
  'At RePay.ph I rewrote the Laravel backend as a Java Spring Boot service: schema design, REST APIs, third-party integrations and the test suite around them. I own the change end to end - migration plan, cutover, and the monitoring that tells us it worked.',
  'current role present repay responsibilities day to day', NULL),

 ('aaaaaaa1-0000-4000-8000-00000000000d', 'background',
  'Why are you looking for a new role?',
  'I want to work on AI systems where the engineering discipline matters - evaluation, guardrails, grounded retrieval - rather than adding a chat widget to a product. My last three years were back end and integration; this is the adjacent step I have been building toward with local RAG and agent tooling.',
  'why looking leaving motivation new role search reason', NULL),

 ('aaaaaaa1-0000-4000-8000-00000000000e', 'engineering',
  'How do you test and ship back-end changes?',
  'Tests at the seam that matters: contract tests for the API, integration tests against a real database in a container, and unit tests for the logic that has branches worth breaking. CI runs them on every push with GitHub Actions, and deploys are one image - if it is not reproducible in Docker it is not done.',
  'testing tests ci cd pipeline deployment docker github actions release', NULL),

 ('aaaaaaa1-0000-4000-8000-00000000000f', 'engineering',
  'How do you keep a Python service in production healthy?',
  'The same way as the Java ones: a typed boundary (pydantic at the edge), structured logs with a request id, health and readiness endpoints, bounded timeouts and retries, and a metric for the thing users notice - p95 latency and error rate. FastAPI behind gunicorn with one worker per core, containerised, restarted by the platform rather than by hand.',
  'python health monitoring observability fastapi gunicorn logging metrics latency', NULL)
ON CONFLICT (id) DO NOTHING;
