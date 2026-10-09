import { Server } from "lucide-react";
import anthropic from "@lobehub/icons-static-svg/icons/anthropic.svg?raw";
import claude from "@lobehub/icons-static-svg/icons/claude.svg?raw";
import gemini from "@lobehub/icons-static-svg/icons/gemini.svg?raw";
import ollama from "@lobehub/icons-static-svg/icons/ollama.svg?raw";
import openai from "@lobehub/icons-static-svg/icons/openai.svg?raw";
import openrouter from "@lobehub/icons-static-svg/icons/openrouter.svg?raw";
import zai from "@lobehub/icons-static-svg/icons/zai.svg?raw";
import deepseek from "@lobehub/icons-static-svg/icons/deepseek.svg?raw";
import fireworks from "@lobehub/icons-static-svg/icons/fireworks.svg?raw";
import groq from "@lobehub/icons-static-svg/icons/groq.svg?raw";
import lmstudio from "@lobehub/icons-static-svg/icons/lmstudio.svg?raw";
import mistral from "@lobehub/icons-static-svg/icons/mistral.svg?raw";
import moonshot from "@lobehub/icons-static-svg/icons/moonshot.svg?raw";
import together from "@lobehub/icons-static-svg/icons/together.svg?raw";
import vllm from "@lobehub/icons-static-svg/icons/vllm.svg?raw";
import xai from "@lobehub/icons-static-svg/icons/xai.svg?raw";
import { get } from "../store";

// A Map, not an object literal: an object inherits Object.prototype, so
// ICON_ASSETS["constructor"] or ["toString"] returns a truthy non-SVG that
// this component would inject through dangerouslySetInnerHTML. The key comes
// from backend provider data, so it is not under our control. This is the only
// dangerouslySetInnerHTML in the app — everything else is React-rendered.
const ICON_ASSETS = new Map<string, string>(
  Object.entries({
    anthropic,
    claude,
    gemini,
    ollama,
    openai,
    openrouter,
    zai,
    deepseek,
    fireworks,
    groq,
    lmstudio,
    mistral,
    moonshot,
    together,
    vllm,
    xai,
  }),
);

export function ProviderIcon({ provider, icon, size = 16, className = "" }: { provider: string; icon?: string; size?: number; className?: string }) {
  const asset = icon ?? get().providers.find((p) => p.id === provider)?.icon ?? provider;
  const svg = ICON_ASSETS.get(asset);
  if (!svg) return <Server aria-hidden="true" className={className} size={size} strokeWidth={1.7} />;
  return <span aria-hidden="true" className={`provider-icon ${className}`} style={{ width: size, height: size }} dangerouslySetInnerHTML={{ __html: svg }} />;
}
