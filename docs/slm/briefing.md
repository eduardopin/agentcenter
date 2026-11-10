# Briefing function card

## Function

`agentcenter-cli insights` shows what needs attention on the machine, most urgent first. A fixed rule engine
produces up to four findings from the snapshot. A two-sentence briefing sits above them. By default the
briefing is the rule text. With `--llm` a small local language model words the same findings instead, and a
guard decides whether its answer may be shown.

The model adds wording only. The rules decide what is urgent, and the numbers come from the snapshot.

## Contract

Input to the model, in this order: a system prompt, one worked example (input and output), and the findings of
this run as numbered lines (`1. [high] ...`). Each line is the rule text of a finding with its severity.

Output: plain text of 20 to 420 characters, the first paragraph only. Two sentences: the most urgent problem
with its numbers, then what to do first, naming the command when one is given.

Command output:

- Text: the briefing, a line `briefing: rules` or `briefing: model`, then the numbered findings. When a model
  answer was rejected or the call failed, the line reads
  `briefing: rules (model answer refused: <reason>)`.
- `--json`: `{ "schema_version": 1, "taken_at_ms", "briefing": { "text", "source", "rejected" }, "findings": [...] }`.
  `source` is `rules` or `model`. `rejected` is null unless a model answer was refused.

## Where it runs

Only in `agentcenter-cli insights`, after sampling, in the same process, once per run. The sampler, `snapshot`,
`watch` and the GUI never call a model. The model server is a separate process that you start yourself.

## Privacy

- The model sees the finding lines only: counts, percentages, sizes, a project directory name and a pid at
  most. It never sees transcript content, prompts, answers or session names.
- Only loopback addresses are accepted (`http://127.0.0.1:PORT`, `http://localhost:PORT`, `http://[::1]:PORT`).
  Any other URL is a usage error with exit code 2, before any sampling or connection. `localhost` is mapped to
  127.0.0.1 without a DNS lookup.
- Without `--llm` and without `AGENTCENTER_LLM` no network call of any kind is made.
- No model weights, binaries or download script live in this repository.

## Fallback

The rule text is shown when there is no `--llm`, when the call fails or exceeds `--llm-timeout-ms` (default
20000), when the server answers with an error status or a malformed body, or when the guard refuses the answer.
The command still exits 0 and the refusal reason is shown.

## The guard

Checks run in this order on the cleaned answer (special tokens and `<think>` tags removed, first paragraph,
whitespace collapsed):

1. Not empty: at least 20 characters.
2. Not too long: at most 420 characters.
3. In role: no phrases of a model that left its task or repeated the prompt ("as an ai", "language model",
   "i cannot", "instructions", "the findings", "sentence 1", and similar).
4. No invented number: every number in the answer appears in the findings (the digits 1 and 2 are always
   allowed). Zeros after a decimal point do not matter.
5. Key facts: the strings the first finding requires (for example the pid and the repeat count) are present,
   with the words one to twelve counted as digits.

## How to start a model

Any OpenAI-compatible server on loopback works. With llama.cpp:

```sh
llama-server -m <file.gguf> --host 127.0.0.1 --port 8080
agentcenter-cli insights --llm http://127.0.0.1:8080
```

The address can also come from `AGENTCENTER_LLM`. The client posts to `/v1/chat/completions` with temperature 0
and at most 120 tokens.

The number check reads every run of numeric characters, fullwidth and other Unicode digits included, as a
number that must appear in the findings (or be 1 or 2), refuses numeric characters with no decimal value, and
refuses spelled numbers above twelve (such as "twenty" or "hundred"), "zero", "dozen" and "billion" unless the
findings contain the same word. The words three to twelve pass only when their digit is in the findings or the
findings contain the same word; "one" and "two" always pass, as 1 and 2 do. The guard also refuses control
characters and invisible format characters (zero width, text direction, tags).
Reasoning text between `<think>` and `</think>` is discarded before the answer is checked.

## Model choice

Measured on 2026-11-10 with the llama.cpp CPU build b12233, 4 threads, on an Intel Core Ultra 7 265U under WSL2.
Each candidate worded the same 60 scenarios, built from one real snapshot with seeded changes. The answers were
checked by the Python reference of the guard, which predates the spelled-number and format-character checks of
the Rust guard. The Rust guard is stricter on those checks and looser on two others (it discards `<think>`
blocks and reads Unicode digits as numbers), so these rates were not measured through it.

| Candidate | Quantization | Size | Guard pass rate (60 scenarios) | p50 | p95 latency on CPU | Peak memory | Decision |
|---|---|---|---|---|---|---|---|
| LFM2.5-350M | Q4_K_M | 229 MB | 1 of 60 (2%) | 1.76 s | 2.98 s | 465 MB | Rejected: pass rate |
| Qwen2.5-0.5B-Instruct | q4_k_m | 491 MB | 47 of 60 (78%) | 2.90 s | 3.87 s | 697 MB | Rejected: pass rate, the closest |
| Qwen3-0.6B, thinking off | Q8_0 | 639 MB | 30 of 60 (50%) | 4.58 s | 6.93 s | 1936 MB | Rejected: pass rate |
| LFM2.5-1.2B-Instruct | Q4_K_M | 731 MB | 8 of 60 (13%) | 5.44 s | 7.77 s | 1313 MB | Rejected: pass rate |
| Qwen2.5-1.5B-Instruct | q4_k_m | 1117 MB | 25 of 60 (42%) | 4.95 s | 7.31 s | 1941 MB | Rejected: pass rate |

No candidate meets the bar, so no model is recommended and the rule text stays the briefing. All five stay
within the latency limit. Of the 189 refused answers, 158 left out a key fact of the first finding (its number
or its action word), 30 carried a number that is not in the findings and 1 was empty. All 13 refusals of
Qwen2.5-0.5B-Instruct were a missing key fact. The next thing to try is a fine-tune of that model on rule text
paired with accepted briefings, not a larger model: LFM2.5-1.2B-Instruct (13%) and Qwen2.5-1.5B-Instruct (42%) both passed less often than it.

Acceptance bar: at least 90% of answers pass the guard on 60 scenarios, and the 95th percentile of the
response time stays within 8 seconds on CPU. The smallest candidate that meets both is the recommended one. If
none does, the rule text stays the only briefing and the feature remains optional.
