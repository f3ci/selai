import { ThemedSelect, type SelectOption } from './ThemedSelect';

export interface PickerOption { value: string; label: string; detail?: string }

/** Compact composer variant of the shared application select. */
export function ComposerPicker(props: {
  label: string; title?: string; value: string; options: PickerOption[];
  disabled?: boolean; className?: string; onChange: (value: string) => void;
}) {
  return <ThemedSelect {...props} options={props.options as SelectOption[]} className={`composer-picker ${props.className ?? ''}`} />;
}
