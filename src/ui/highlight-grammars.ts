/** The highlight.js grammars, in their own module so it can be its own chunk.
 *
 * This is ~160 KB of the startup bundle — 37 `common` grammars plus the extras
 * below — and nothing needs it until an agent's reply contains a fenced code
 * block with a language label. Keeping it out of the entry bundle is the whole
 * point of this file existing; do not import it statically from anywhere.
 *
 * `common` rather than `all` (192 grammars), plus a short list of extras: the
 * list is what agents actually fence here, and PowerShell matters because this
 * is a Windows-first tool while `common` has no grammar for it at all.
 */
import { createLowlight, common } from "lowlight";
/** The highlighter lowlight hands back. lowlight v3 exports no `Lowlight`
 *  type of its own, so derive it rather than restate the shape. */
export type Lowlight = ReturnType<typeof createLowlight>;
import powershell from "highlight.js/lib/languages/powershell";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import haskell from "highlight.js/lib/languages/haskell";
import elixir from "highlight.js/lib/languages/elixir";
import kotlin from "highlight.js/lib/languages/kotlin";
import swift from "highlight.js/lib/languages/swift";
import scala from "highlight.js/lib/languages/scala";
import lua from "highlight.js/lib/languages/lua";
import dart from "highlight.js/lib/languages/dart";
import clojure from "highlight.js/lib/languages/clojure";
import nginx from "highlight.js/lib/languages/nginx";
import latex from "highlight.js/lib/languages/latex";
import julia from "highlight.js/lib/languages/julia";
import graphql from "highlight.js/lib/languages/graphql";
import vbnet from "highlight.js/lib/languages/vbnet";
import verilog from "highlight.js/lib/languages/verilog";

/** Build the highlighter. Called once, by `highlight.ts`, off the startup path. */
export function build(): Lowlight {
  const l = createLowlight(common);
  l.register({
    powershell, dockerfile, haskell, elixir, kotlin, swift, scala, lua, dart,
    clojure, nginx, latex, julia, graphql, vbnet, verilog,
  });
  return l;
}
