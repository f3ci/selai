import { fireEvent, screen, within } from '@testing-library/react';

export function chooseSelectValue(label: string, value: string) {
  fireEvent.click(screen.getByRole('combobox', { name: label }));
  const options = within(screen.getByRole('listbox', { name: label })).getAllByRole('option');
  const option = options.find((item) => item.getAttribute('data-value') === value);
  if (!option) throw new Error(`No option with value ${value} in ${label}`);
  fireEvent.click(option);
}
