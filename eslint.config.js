// OpenLeash ESLint flat config.
//
// Deliberately MINIMAL and non-disruptive. See the per-rule comments for the
// reasoning behind every opt-in and opt-out.
//
//   * Not type-aware. Type correctness is already enforced by `npx tsc --noEmit`,
//     which is the stricter and much faster gate. Type-aware linting would roughly
//     double the type-check cost for no extra signal.
//   * No stylistic rules. This codebase uses dense one-line JSX and a hand-rolled
//     visual system; Prettier-style enforcement would reject the existing style
//     wholesale. Formatting is not linted here.
//   * `react-hooks` is the one plugin worth having: `rules-of-hooks` is a real
//     correctness rule, and `exhaustive-deps` is downgraded to "warn" on purpose —
//     several effects here have intentional dependency lists (see the comment above
//     the subscription in src/ui/Session.tsx).
//
// `tsc --noEmit` and `vitest run` remain the source of truth for correctness.
// This config is a net for the bugs a type-checker cannot see.

import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";

export default tseslint.config(
  {
    // Build output, the Rust crate, generated Tauri schemas, vendored binaries.
    ignores: [
      "dist/**",
      "node_modules/**",
      "compiled/**",
      "src-tauri/**",
      "src/gen/**",
    ],
  },

  js.configs.recommended,
  ...tseslint.configs.recommended,

  // Browser globals for app + test code.
  {
    files: ["src/**/*.{ts,tsx}"],
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: {
        ...globals.browser,
        ...globals.es2021,
      },
    },
    plugins: {
      "react-hooks": reactHooks,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,

      // ---- the two rules we actually want, at the severities we want --------
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "warn",

      // ---- the React Compiler rule set: opinionated, and it fires on idioms
      // this codebase uses deliberately. Off by default; opt in per file if you
      // are actually trying to migrate to the compiler. -------------------------
      "react-hooks/immutability": "off",
      "react-hooks/purity": "off",
      "react-hooks/globals": "off",
      "react-hooks/refs": "off",
      "react-hooks/set-state-in-effect": "off",
      "react-hooks/set-state-in-render": "off",
      "react-hooks/no-deriving-state-in-effects": "off",
      "react-hooks/incompatible-library": "off",
      "react-hooks/static-components": "off",
      "react-hooks/use-memo": "off",
      "react-hooks/void-use-memo": "off",
      "react-hooks/preserve-manual-memoization": "off",
      "react-hooks/memo-dependencies": "off",
      "react-hooks/memoized-effect-dependencies": "off",
      "react-hooks/exhaustive-effect-dependencies": "off",
      "react-hooks/error-boundaries": "off",
      "react-hooks/unsupported-syntax": "off",
      "react-hooks/component-hook-factories": "off",

      // `no-undef` is off for TS: TypeScript already reports genuinely undefined
      // identifiers, and ESLint's rule has no knowledge of the TS type space.
      "no-undef": "off",

      // These three fight intentional, readable code rather than catching bugs:
      //   * `no-control-regex` — src/ui/Session.tsx:787 matches a literal \x08
      //     byte inside a provider error string. That is the point of the regex.
      //   * `no-useless-escape`  — src/ui/Settings.tsx:234 escapes `/` inside a
      //     character class, which is legal and matches the surrounding style.
      //   * `no-unused-expressions` — the codebase uses `cond && fn()` as a
      //     short-circuit call (Chrome.tsx:140, Overlays.tsx:379/534).
      // They stay configurable; these are just the defaults.
      "no-control-regex": "off",
      "no-useless-escape": "off",
      "@typescript-eslint/no-unused-expressions": "off",

      // `any` is a deliberate, load-bearing choice at the Tauri IPC boundary:
      // payloads arrive as untyped JSON and the 34 occurrences are concentrated in
      // the API/test/parsing helpers (api.ts, store.ts, ui.test.ts, Session.tsx,
      // Question.tsx, ...). `tsc --noEmit` passes with them, so this is advisory
      // noise for day one. Downgraded to "warn" rather than removed: 34 sites
      // should shrink over time, and new `any` should be noticeable. Flip back to
      // "error" once the existing ones are gone.
      "@typescript-eslint/no-explicit-any": "warn",

      // `tsc` runs with `noUnusedLocals` / `noUnusedParameters` and is stricter
      // than either ESLint rule. Keep ESLint's version at "warn" with `_` escapes
      // allowed so it can still catch a genuinely dead import on a config path
      // that skips the type-check.
      "no-unused-vars": "off",
      "@typescript-eslint/no-unused-vars": [
        "warn",
        {
          argsIgnorePattern: "^_",
          varsIgnorePattern: "^_",
          caughtErrorsIgnorePattern: "^_",
          destructuredArrayIgnorePattern: "^_",
        },
      ],
    },
  },
);
