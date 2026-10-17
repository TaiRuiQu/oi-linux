import type { ReactNode } from "react";
import { useTranslation } from "react-i18next";
import type { ShareStatus, VmStatus } from "../types";

export type Tone = "grey" | "green" | "red" | "amber" | "blue";

export function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return <span className={`badge badge--${tone}`}>{children}</span>;
}

export function vmTone(status: VmStatus): Tone {
  switch (status) {
    case "running":
    case "starting":
      return "green";
    case "stopping":
      return "amber";
    case "start_error":
      return "red";
    case "stopped":
    case "uninitialized":
    default:
      return "grey";
  }
}

export function shareTone(status: ShareStatus): Tone {
  switch (status) {
    case "mounted":
      return "green";
    case "mounting":
    case "recovering":
      return "amber";
    case "degraded":
      return "amber";
    case "error":
      return "red";
    case "not_mounted":
    default:
      return "grey";
  }
}

export function VmStateBadge({ status }: { status: VmStatus }) {
  const { t } = useTranslation();
  return <Badge tone={vmTone(status)}>{t(`state.vm.${status}`)}</Badge>;
}

export function ShareStateBadge({ status }: { status: ShareStatus }) {
  const { t } = useTranslation();
  return <Badge tone={shareTone(status)}>{t(`state.share.${status}`)}</Badge>;
}

export function Card({
  title,
  description,
  actions,
  children,
}: {
  title: string;
  description?: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="card">
      <header className="cardHeader">
        <div>
          <h2>{title}</h2>
          {description ? <p className="cardDescription">{description}</p> : null}
        </div>
        {actions ? <div className="cardActions">{actions}</div> : null}
      </header>
      <div className="cardBody">{children}</div>
    </section>
  );
}

export function DefList({ children }: { children: ReactNode }) {
  return <dl className="defList">{children}</dl>;
}

export function DefRow({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="defRow">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

export function PathValue({ value }: { value: string | null | undefined }) {
  const text = value && value.length > 0 ? value : "—";
  return (
    <span className="pathValue" title={text}>
      {text}
    </span>
  );
}

export function Mono({ children, title }: { children: ReactNode; title?: string }) {
  return (
    <span className="mono" title={title}>
      {children}
    </span>
  );
}

export function Hint({
  tone = "info",
  children,
}: {
  tone?: "info" | "warn" | "danger";
  children: ReactNode;
}) {
  return <p className={`hint hint--${tone}`}>{children}</p>;
}

export function Button({
  children,
  onClick,
  variant = "default",
  disabled = false,
  label,
  title,
}: {
  children: ReactNode;
  onClick: () => void;
  variant?: "default" | "primary" | "danger";
  disabled?: boolean;
  label?: string;
  title?: string;
}) {
  const variantClass =
    variant === "primary" ? " button--primary" : variant === "danger" ? " button--danger" : "";
  return (
    <button
      type="button"
      className={`button${variantClass}`}
      onClick={onClick}
      disabled={disabled}
      aria-label={label}
      title={title}
    >
      {children}
    </button>
  );
}

export function CheckRow({
  checked,
  onChange,
  children,
}: {
  checked: boolean;
  onChange: (next: boolean) => void;
  children: ReactNode;
}) {
  return (
    <label className="checkRow">
      <input
        type="checkbox"
        checked={checked}
        onChange={(event) => onChange(event.target.checked)}
      />
      <span>{children}</span>
    </label>
  );
}

export function TextInput({
  id,
  value,
  onChange,
  disabled = false,
  placeholder,
}: {
  id: string;
  value: string;
  onChange: (next: string) => void;
  disabled?: boolean;
  placeholder?: string;
}) {
  return (
    <input
      id={id}
      className="textInput"
      type="text"
      spellCheck={false}
      autoComplete="off"
      value={value}
      disabled={disabled}
      placeholder={placeholder}
      onChange={(event) => onChange(event.target.value)}
    />
  );
}

export function NumberInput({
  id,
  value,
  onChange,
  min,
  max,
  step = 1,
  disabled = false,
}: {
  id: string;
  value: number;
  onChange: (next: number) => void;
  min: number;
  max: number;
  step?: number;
  disabled?: boolean;
}) {
  return (
    <input
      id={id}
      className="numberInput"
      type="number"
      inputMode="numeric"
      value={Number.isFinite(value) ? value : min}
      min={min}
      max={max}
      step={step}
      disabled={disabled}
      onChange={(event) => {
        const parsed = Number.parseInt(event.target.value, 10);
        onChange(Number.isNaN(parsed) ? min : parsed);
      }}
    />
  );
}

export function StatusLine({ label, tone, value }: { label: string; tone: Tone; value: string }) {
  return (
    <div className="statusLine">
      <span className="statusLineLabel">{label}</span>
      <Badge tone={tone}>{value}</Badge>
    </div>
  );
}
