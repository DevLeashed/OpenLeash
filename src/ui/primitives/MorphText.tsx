import { CSSProperties, ElementType, ReactNode } from "react";
import { TextMorph } from "torph/react";
import "./MorphText.css";

export function MorphText({ children, as = "span", className, style }: { children: ReactNode; as?: ElementType; className?: string; style?: CSSProperties }) {
  return (
    <TextMorph as={as} className={["morph-text", className].filter(Boolean).join(" ")} style={style} duration={180} ease="cubic-bezier(0.19, 1, 0.22, 1)" scale={false} respectReducedMotion>
      {children}
    </TextMorph>
  );
}
