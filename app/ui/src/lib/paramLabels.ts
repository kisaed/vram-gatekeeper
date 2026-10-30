/**
 * Readable names and descriptions for llama-server parameters (spec §10.2, §23.2).
 *
 * The parameter catalog is generated from the installed engine's `--help`, so its
 * names are command-line flags. The tables below give the common ones a formal,
 * readable name. Anything the tables do not know (a newer engine, say) gets a name
 * derived from its long flag, so no raw flag is ever the title. The original flags
 * stay available in the hover text and in search.
 */
import type { ParamSpec } from './types';

/** Section names from the engine's help, in the words shown above each group. */
export const SECTION_LABEL: Record<string, string> = {
  common: 'Model Loading and Performance',
  sampling: 'Sampling and Generation',
  speculative: 'Speculative Decoding',
  'example-specific': 'Server and Chat Options',
};

/** Readable name for a section, falling back to Title Case of the engine's name. */
export function sectionLabel(section: string): string {
  return SECTION_LABEL[section] ?? titleCase(section.replaceAll('_', '-').split('-'));
}

/** Names keyed by the long flag without its dashes. */
const LABELS: Record<string, string> = {
  // Model loading and performance
  'cache-list': 'List Cached Models',
  threads: 'Generation Threads',
  'threads-batch': 'Prompt Processing Threads',
  'cpu-mask': 'CPU Affinity Mask',
  'cpu-range': 'CPU Affinity Range',
  'cpu-strict': 'Strict CPU Placement',
  prio: 'Process Priority',
  poll: 'Polling Level',
  'cpu-mask-batch': 'Prompt Processing CPU Affinity Mask',
  'cpu-range-batch': 'Prompt Processing CPU Affinity Range',
  'cpu-strict-batch': 'Prompt Processing Strict CPU Placement',
  'prio-batch': 'Prompt Processing Priority',
  'poll-batch': 'Prompt Processing Polling Level',
  'ctx-size': 'Context Size',
  predict: 'Maximum Tokens to Generate',
  'n-predict': 'Maximum Tokens to Generate',
  'batch-size': 'Maximum Logical Batch Size',
  'ubatch-size': 'Maximum Physical Batch Size',
  keep: 'Initial Prompt Tokens to Keep',
  'swa-full': 'Full-Size Sliding Window Cache',
  'flash-attn': 'Flash Attention',
  perf: 'Performance Timing',
  escape: 'Process Escape Sequences',
  'rope-scaling': 'RoPE Scaling Method',
  'rope-scale': 'RoPE Context Scaling Factor',
  'rope-freq-base': 'RoPE Base Frequency',
  'rope-freq-scale': 'RoPE Frequency Scaling Factor',
  'yarn-orig-ctx': 'YaRN Original Context Size',
  'yarn-ext-factor': 'YaRN Extrapolation Mix Factor',
  'yarn-attn-factor': 'YaRN Attention Magnitude Factor',
  'yarn-beta-slow': 'YaRN High Correction Dimension',
  'yarn-beta-fast': 'YaRN Low Correction Dimension',
  'kv-offload': 'KV Cache GPU Offloading',
  repack: 'Weight Repacking',
  'no-host': 'Bypass Host Buffer',
  'cache-type-k': 'KV Cache Key Data Type',
  'cache-type-v': 'KV Cache Value Data Type',
  'defrag-thold': 'KV Cache Defragmentation Threshold',
  rpc: 'Remote Procedure Call Servers',
  'load-mode': 'Model Loading Mode',
  'lazy-mode': 'Lazy Tensor Loading Mode',
  numa: 'NUMA Optimization',
  device: 'GPU Devices',
  'override-tensor': 'Tensor Buffer Type Overrides',
  'cpu-moe': 'Keep All Expert Weights on CPU',
  'n-cpu-moe': 'Expert Weight Layers on CPU',
  'moe-cache-mib': 'Expert Weight GPU Cache Size',
  'n-cpu-ffn': 'Feed-Forward Layers on CPU',
  'gpu-layers': 'GPU Layers',
  'n-gpu-layers': 'GPU Layers',
  'split-mode': 'Multi-GPU Split Mode',
  'tensor-split': 'Multi-GPU Tensor Split',
  'main-gpu': 'Primary GPU',
  fit: 'Automatic Memory Fitting',
  'fit-target': 'Memory Fitting Margin',
  'fit-ctx': 'Minimum Context Size for Fitting',
  'check-tensors': 'Validate Tensor Data',
  'override-kv': 'Model Metadata Overrides',
  'op-offload': 'Host Operation Offloading',
  lora: 'LoRA Adapters',
  'lora-scaled': 'Scaled LoRA Adapters',
  'control-vector': 'Control Vectors',
  'control-vector-scaled': 'Scaled Control Vectors',
  'control-vector-layer-range': 'Control Vector Layer Range',
  'log-jsonl': 'JSON Lines Logging',
  verbose: 'Maximum Log Verbosity',
  offline: 'Offline Mode',
  verbosity: 'Log Verbosity Level',
  'spec-draft-type-k': 'Draft Model KV Cache Key Data Type',
  'spec-draft-type-v': 'Draft Model KV Cache Value Data Type',

  // Sampling and generation
  samplers: 'Sampler Order',
  seed: 'Random Seed',
  'sampler-seq': 'Sampler Sequence',
  'ignore-eos': 'Ignore End-of-Stream Token',
  temp: 'Temperature',
  'top-k': 'Top-K Sampling',
  'top-p': 'Top-P Sampling',
  'min-p': 'Minimum-P Sampling',
  'top-nsigma': 'Top-N-Sigma Sampling',
  'xtc-probability': 'XTC Probability',
  'xtc-threshold': 'XTC Threshold',
  typical: 'Locally Typical Sampling',
  'repeat-last-n': 'Repetition Penalty Window',
  'repeat-penalty': 'Repetition Penalty',
  'presence-penalty': 'Presence Penalty',
  'frequency-penalty': 'Frequency Penalty',
  'dry-multiplier': 'DRY Penalty Multiplier',
  'dry-base': 'DRY Penalty Base',
  'dry-allowed-length': 'DRY Allowed Sequence Length',
  'dry-penalty-last-n': 'DRY Penalty Window',
  'dry-sequence-breaker': 'DRY Sequence Breakers',
  'adaptive-target': 'Adaptive-P Target Probability',
  'adaptive-decay': 'Adaptive-P Decay Rate',
  'dynatemp-range': 'Dynamic Temperature Range',
  'dynatemp-exp': 'Dynamic Temperature Exponent',
  mirostat: 'Mirostat Mode',
  'mirostat-lr': 'Mirostat Learning Rate',
  'mirostat-ent': 'Mirostat Target Entropy',
  'logit-bias': 'Token Probability Bias',
  grammar: 'Output Grammar',
  'grammar-file': 'Output Grammar File',
  'json-schema': 'JSON Schema Constraint',
  'json-schema-file': 'JSON Schema File',
  'backend-sampling': 'Backend Sampling (Experimental)',

  // Speculative decoding
  'spec-draft-hf': 'Draft Model Hugging Face Repository',
  'spec-draft-threads': 'Draft Model Generation Threads',
  'spec-draft-threads-batch': 'Draft Model Prompt Processing Threads',
  'spec-draft-cpu-mask': 'Draft Model CPU Affinity Mask',
  'spec-draft-cpu-range': 'Draft Model CPU Affinity Range',
  'spec-draft-cpu-strict': 'Draft Model Strict CPU Placement',
  'spec-draft-prio': 'Draft Model Process Priority',
  'spec-draft-poll': 'Draft Model Polling Level',
  'spec-draft-cpu-mask-batch': 'Draft Model Prompt Processing CPU Affinity Mask',
  'spec-draft-cpu-strict-batch': 'Draft Model Prompt Processing Strict CPU Placement',
  'spec-draft-prio-batch': 'Draft Model Prompt Processing Priority',
  'spec-draft-poll-batch': 'Draft Model Prompt Processing Polling Level',
  'spec-draft-override-tensor': 'Draft Model Tensor Buffer Type Overrides',
  'spec-draft-cpu-moe': 'Draft Model Expert Weights on CPU',
  'spec-draft-n-cpu-moe': 'Draft Model Expert Weight Layers on CPU',
  'spec-draft-n-max': 'Maximum Draft Tokens',
  'spec-draft-n-min': 'Minimum Draft Tokens',
  'spec-draft-n-ubatch': 'Draft Model Physical Batch Size',
  'spec-synth-len': 'Synthetic Acceptance Length',
  'spec-synth-rates': 'Synthetic Acceptance Rates',
  'spec-draft-p-split': 'Draft Split Probability',
  'spec-draft-p-min': 'Minimum Draft Probability',
  'spec-draft-backend-sampling': 'Draft Model Backend Sampling',
  'spec-draft-sampling': 'Draft Sampling Method',
  'spec-draft-device': 'Draft Model GPU Devices',
  'spec-draft-ngl': 'Draft Model GPU Layers',
  'spec-draft-model': 'Draft Model File',
  'spec-type': 'Speculative Decoding Types',
  'spec-ngram-mod-n-min': 'N-Gram Module Minimum Tokens',
  'spec-ngram-mod-n-max': 'N-Gram Module Maximum Tokens',
  'spec-ngram-mod-n-match': 'N-Gram Module Lookup Length',
  'spec-ngram-simple-size-n': 'Simple N-Gram Lookup Length',
  'spec-ngram-simple-size-m': 'Simple N-Gram Draft Length',
  'spec-ngram-simple-min-hits': 'Simple N-Gram Minimum Hits',
  'spec-ngram-map-k-size-n': 'N-Gram Map (Keys) Lookup Length',
  'spec-ngram-map-k-size-m': 'N-Gram Map (Keys) Draft Length',
  'spec-ngram-map-k-min-hits': 'N-Gram Map (Keys) Minimum Hits',
  'spec-ngram-map-k4v-size-n': 'N-Gram Map (Keys and Values) Lookup Length',
  'spec-ngram-map-k4v-size-m': 'N-Gram Map (Keys and Values) Draft Length',
  'spec-ngram-map-k4v-min-hits': 'N-Gram Map (Keys and Values) Minimum Hits',
  draft: 'Maximum Draft Tokens (Removed)',
  'draft-min': 'Minimum Draft Tokens (Removed)',
  'spec-ngram-size-n': 'N-Gram Lookup Length (Removed)',
  'spec-ngram-size-m': 'N-Gram Draft Length (Removed)',
  'spec-ngram-min-hits': 'N-Gram Minimum Hits (Removed)',
  'spec-default': 'Default Speculative Decoding',

  // Server and chat options
  'lookup-cache-static': 'Static Lookup Cache File',
  'lookup-cache-dynamic': 'Dynamic Lookup Cache File',
  'kv-unified-per-slot': 'Context Limit per Parallel Slot',
  'ctx-checkpoints': 'Context Checkpoints per Slot',
  'checkpoint-min-step': 'Minimum Checkpoint Spacing',
  'cache-ram': 'Prompt Cache Size Limit',
  'kv-unified': 'Unified KV Buffer',
  'cache-idle-slots': 'Cache Idle Slots',
  'context-shift': 'Context Shifting',
  'reverse-prompt': 'Stop Prompt',
  special: 'Output Special Tokens',
  warmup: 'Warm-Up Run',
  'spm-infill': 'Suffix-Prefix-Middle Infill Pattern',
  pooling: 'Embedding Pooling Type',
  parallel: 'Parallel Request Slots',
  'cont-batching': 'Continuous Batching',
  'mmproj-url': 'Vision Projector URL',
  'mmproj-auto': 'Use Vision Projector Automatically',
  'mmproj-offload': 'Vision Projector GPU Offloading',
  'mmproj-device': 'Vision Projector GPU Device',
  'image-min-tokens': 'Minimum Tokens per Image',
  'image-max-tokens': 'Maximum Tokens per Image',
  'mtmd-batch-max-tokens': 'Maximum Image Tokens per Batch',
  'video-fps': 'Video Frame Rate',
  'video-timestamp-interval': 'Video Timestamp Interval',
  'video-ffmpeg-dir': 'FFmpeg Directory',
  alias: 'Model Name Aliases',
  tags: 'Model Tags',
  'embd-normalize': 'Embedding Normalization',
  'reuse-port': 'Allow Shared Port',
  'cors-origins': 'Allowed CORS Origins',
  'cors-methods': 'Allowed CORS Methods',
  'cors-headers': 'Allowed CORS Headers',
  'cors-credentials': 'Allow CORS Credentials',
  'api-prefix': 'API Path Prefix',
  'ui-config': 'Web Interface Default Settings',
  'ui-config-file': 'Web Interface Settings File',
  'ui-mcp-proxy': 'Web Interface MCP Proxy (Experimental)',
  tools: 'Built-In Agent Tools (Experimental)',
  'tools-runtime': 'Agent Tool Runtime (Experimental)',
  'mcp-servers-config': 'MCP Servers Configuration File (Experimental)',
  'mcp-servers-json': 'MCP Servers Configuration (Experimental)',
  agent: 'Agent Mode (Experimental)',
  ui: 'Built-In Web Interface',
  connect: 'Peer-to-Peer Tunnel (Experimental)',
  'connect-code': 'Tunnel Connection Code',
  embedding: 'Embedding-Only Mode',
  rerank: 'Reranking Endpoint',
  'chat-template-kwargs': 'Chat Template Parameters',
  timeout: 'Server Timeout',
  'sse-ping-interval': 'Event Stream Keep-Alive Interval',
  'threads-http': 'HTTP Request Threads',
  'cache-prompt': 'Prompt Caching',
  'cache-reuse': 'Minimum Cache Reuse Chunk Size',
  metrics: 'Metrics Endpoint',
  props: 'Global Properties Endpoint',
  slots: 'Slot Monitoring Endpoint',
  'slot-save-path': 'Slot Cache Save Path',
  'media-path': 'Local Media Directory',
  'models-autoload': 'Automatically Load Models',
  jinja: 'Jinja Chat Templates',
  'reasoning-format': 'Reasoning Output Format',
  reasoning: 'Reasoning Mode',
  'reasoning-effort': 'Reasoning Effort',
  'reasoning-budget': 'Reasoning Token Budget',
  'reasoning-budget-message': 'Reasoning Budget Message',
  'reasoning-preserve': 'Preserve Reasoning History',
  'chat-template': 'Chat Template',
  'chat-template-file': 'Chat Template File',
  'skip-chat-parsing': 'Skip Chat Parsing',
  'prefill-assistant': 'Prefill Assistant Response',
  'slot-prompt-similarity': 'Slot Prompt Similarity Threshold',
  'lora-init-without-apply': 'Load LoRA Adapters Without Applying',
  'log-prompts-dir': 'Prompt Log Directory',
  'embd-gemma-default': 'Preset: EmbeddingGemma',
  'fim-qwen-1.5b-default': 'Preset: Qwen 2.5 Coder 1.5B',
  'fim-qwen-3b-default': 'Preset: Qwen 2.5 Coder 3B',
  'fim-qwen-7b-default': 'Preset: Qwen 2.5 Coder 7B',
  'fim-qwen-7b-spec': 'Preset: Qwen 2.5 Coder 7B with Draft Model',
  'fim-qwen-14b-spec': 'Preset: Qwen 2.5 Coder 14B with Draft Model',
  'fim-qwen-30b-default': 'Preset: Qwen 3 Coder 30B A3B Instruct',
  'gpt-oss-20b-default': 'Preset: gpt-oss 20B',
  'gpt-oss-120b-default': 'Preset: gpt-oss 120B',
  'vision-gemma-4b-default': 'Preset: Gemma 3 4B QAT',
  'vision-gemma-12b-default': 'Preset: Gemma 3 12B QAT',
};

/** Hand-written explanations for the settings people change most. Shown on hover. */
const DESCRIPTIONS: Record<string, string> = {
  threads: 'Number of CPU threads used while generating text. A value of -1 selects the count automatically.',
  'threads-batch': 'Number of CPU threads used while processing the prompt. By default, the generation thread count is used.',
  'ctx-size': 'Size of the context window in tokens, which bounds the combined length of the prompt and the response. A value of 0 uses the size stored in the model. Larger values consume more memory.',
  predict: 'Maximum number of tokens generated for a single response. A value of -1 removes the limit.',
  'batch-size': 'Largest number of tokens submitted to the engine in one logical batch while processing a prompt.',
  'ubatch-size': 'Largest number of tokens processed in one physical batch. Lower values reduce peak memory use.',
  'flash-attn': 'Enables the Flash Attention optimization, which lowers memory use and speeds up long contexts. "Auto" lets the engine decide.',
  'rope-scaling': 'Method used to extend the context beyond the length the model was trained on.',
  'rope-scale': 'Factor by which the context length is extended through RoPE scaling.',
  'kv-offload': 'Keeps the KV cache (the model\'s working memory for the conversation) on the GPU. Disable it to save GPU memory at the cost of speed.',
  'cache-type-k': 'Numeric precision of the KV cache keys. Lower precision reduces memory use and may slightly reduce quality.',
  'cache-type-v': 'Numeric precision of the KV cache values. Lower precision reduces memory use and may slightly reduce quality.',
  device: 'Comma-separated list of GPU devices used for offloading. Enter "none" to run entirely on the CPU.',
  'gpu-layers': 'Number of model layers stored in GPU memory. Enter a number, "auto", or "all". More layers on the GPU generally means faster generation.',
  'split-mode': 'Method used to distribute the model across several GPUs.',
  'tensor-split': 'Proportion of the model placed on each GPU, as a comma-separated list, for example 3,1.',
  'main-gpu': 'GPU that holds the model when the split mode is "none", or that stores intermediate results otherwise.',
  fit: 'Allows the engine to adjust settings that were left unset so that the model fits in the available device memory.',
  lora: 'Path to a LoRA adapter file. Separate several paths with commas to load more than one.',
  seed: 'Seed for the random number generator. A value of -1 selects a random seed for every request.',
  temp: 'Sampling temperature. Higher values produce more varied output; lower values produce more predictable output.',
  'top-k': 'Limits sampling to the K most probable tokens. A value of 0 disables the limit.',
  'top-p': 'Limits sampling to the smallest set of tokens whose combined probability reaches this value. A value of 1.0 disables the limit.',
  'min-p': 'Excludes tokens whose probability is below this fraction of the most probable token. A value of 0.0 disables the limit.',
  'repeat-penalty': 'Penalty applied to tokens that recently appeared, which discourages repetition. A value of 1.0 disables the penalty.',
  pooling: 'Method used to combine token embeddings into a single vector. When unset, the model default is used.',
  parallel: 'Number of requests the server can process at the same time. A value of -1 selects the count automatically. The context is shared between slots.',
  'cont-batching': 'Allows new requests to join a batch that is already running, which improves throughput when several clients are connected.',
  ui: 'Enables the web interface that llama-server serves on its own port.',
  embedding: 'Restricts the server to embedding requests. Use only with models designed for embeddings.',
  rerank: 'Enables the reranking endpoint on the server.',
  'cache-reuse': 'Smallest chunk of a previous prompt that may be reused from the cache. A value of 0 disables reuse.',
  'slot-save-path': 'Folder in which the contents of each slot\'s KV cache can be saved.',
  jinja: 'Uses the Jinja template engine to format chat messages. Required for tool calling with most models.',
  'reasoning-format': 'Controls whether the model\'s reasoning ("thinking") text is returned separately from its answer.',
  'chat-template': 'Custom Jinja chat template. When unset, the template stored in the model is used.',
};

/** Words kept in capitals or in their conventional spelling when a name is derived. */
const WORD: Record<string, string> = {
  api: 'API',
  cpu: 'CPU',
  gpu: 'GPU',
  kv: 'KV',
  moe: 'MoE',
  ffn: 'FFN',
  rope: 'RoPE',
  yarn: 'YaRN',
  lora: 'LoRA',
  mcp: 'MCP',
  cors: 'CORS',
  http: 'HTTP',
  sse: 'SSE',
  ui: 'Interface',
  url: 'URL',
  json: 'JSON',
  hf: 'Hugging Face',
  ctx: 'Context',
  n: 'Number of',
  ngl: 'GPU Layers',
  ubatch: 'Physical Batch',
  mmproj: 'Vision Projector',
  spec: 'Speculative',
  draft: 'Draft',
  numa: 'NUMA',
  rpc: 'RPC',
  fim: 'FIM',
  gguf: 'GGUF',
  ram: 'RAM',
  mib: 'MiB',
  fps: 'FPS',
  ffmpeg: 'FFmpeg',
  swa: 'SWA',
  dry: 'DRY',
  xtc: 'XTC',
  eos: 'EOS',
  sm: 'SM',
};

function titleCase(words: string[]): string {
  return words
    .filter(Boolean)
    .map((w) => WORD[w.toLowerCase()] ?? w.charAt(0).toUpperCase() + w.slice(1))
    .join(' ');
}

const norm = (flag: string): string => flag.replace(/^-+/, '');

/** Names to look a parameter up under: its key, then each flag without dashes. */
function lookupKeys(spec: ParamSpec): string[] {
  return [spec.key, ...spec.flags.map(norm)];
}

/** Readable name for a parameter, never a raw command-line flag. */
export function paramLabel(spec: ParamSpec): string {
  for (const k of lookupKeys(spec)) {
    const hit = LABELS[k];
    if (hit) return hit;
  }
  // Derive from the first long flag that is not a negation.
  const long = spec.flags.map(norm).find((f, i) => spec.flags[i].startsWith('--') && !f.startsWith('no-')) ?? spec.key;
  return titleCase(long.split('-'));
}

/** Replaces command-line flags inside engine text (for example "same as --threads") with readable names. */
export function humanizeFlags(text: string, params: ParamSpec[]): string {
  return text.replace(/(?<![\w-])--?[a-z][\w-]*/gi, (flag) => {
    const spec = params.find((p) => p.flags.includes(flag));
    return spec ? paramLabel(spec) : flag;
  });
}

/** Capitalises, drops markdown links and closes the sentence of an engine description. */
export function tidyDescription(text: string): string {
  const plain = text
    .replace(/\s*\[\(more info\)\]\([^)]*\)/gi, '')
    .replace(/\[([^\]]*)\]\([^)]*\)/g, '$1')
    .replace(/\s+/g, ' ')
    .trim();
  if (!plain) return '';
  const first = plain.charAt(0).toUpperCase() + plain.slice(1);
  return /[.!?)]$/.test(first) ? first : `${first}.`;
}

/** Hover text for a parameter: the explanation, the default, and the original command-line option. */
export function paramHelp(spec: ParamSpec, all: ParamSpec[] = []): string {
  const curated = lookupKeys(spec).map((k) => DESCRIPTIONS[k]).find(Boolean);
  const parts = [humanizeFlags(curated ?? tidyDescription(spec.description), all)];
  // The engine's own wording often states the default already.
  if (curated || !/default/i.test(parts[0])) parts.push(spec.default ? `Default: ${humanizeFlags(spec.default, all)}.` : 'No default value.');
  parts.push(`Command-line option: ${spec.flags.join(', ')}${spec.placeholder ? ` ${spec.placeholder}` : ''}`);
  return parts.filter(Boolean).join('\n');
}
