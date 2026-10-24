import type { HostView } from '../ipc';
import { TerminalIcon } from './icons';
import powershell from '../assets/local-environments/powershell.svg';
import debian from '../assets/local-environments/debian.svg';
import ubuntu from '../assets/local-environments/ubuntu.svg';
import docker from '../assets/local-environments/docker.svg';
import fedora from '../assets/local-environments/fedora.svg';
import archlinux from '../assets/local-environments/archlinux.svg';
import kalilinux from '../assets/local-environments/kalilinux.svg';
import opensuse from '../assets/local-environments/opensuse.svg';
import centos from '../assets/local-environments/centos.svg';
import redhat from '../assets/local-environments/redhat.svg';
import rockylinux from '../assets/local-environments/rockylinux.svg';
import almalinux from '../assets/local-environments/almalinux.svg';
import linuxmint from '../assets/local-environments/linuxmint.svg';
import nixos from '../assets/local-environments/nixos.svg';
import gentoo from '../assets/local-environments/gentoo.svg';
import linux from '../assets/local-environments/linux.svg';

const DISTRIBUTIONS: [RegExp, string, string][] = [
  [/docker/i, 'Docker', docker],
  [/ubuntu/i, 'Ubuntu', ubuntu],
  [/debian/i, 'Debian', debian],
  [/kali/i, 'Kali Linux', kalilinux],
  [/fedora/i, 'Fedora', fedora],
  [/arch/i, 'Arch Linux', archlinux],
  [/opensuse|\bsuse\b|\bsles\b/i, 'openSUSE', opensuse],
  [/centos/i, 'CentOS', centos],
  [/red.?hat|\brhel\b/i, 'Red Hat', redhat],
  [/rocky/i, 'Rocky Linux', rockylinux],
  [/alma/i, 'AlmaLinux', almalinux],
  [/mint/i, 'Linux Mint', linuxmint],
  [/nixos/i, 'NixOS', nixos],
  [/gentoo/i, 'Gentoo', gentoo],
];

type Props = { host: Pick<HostView, 'label' | 'wslDistribution'>; size?: number };

/** Bundled brand marks; imported/custom distro names have a Linux fallback. */
export function LocalEnvironmentIcon({ host, size = 18 }: Props) {
  const distro = host.wslDistribution;
  let name: string;
  let source: string;
  if (distro) {
    const distribution = DISTRIBUTIONS.find(([pattern]) => pattern.test(distro));
    name = distribution?.[1] ?? 'Linux';
    source = distribution?.[2] ?? linux;
  } else if (/powershell/i.test(host.label)) {
    name = 'PowerShell'; source = powershell;
  } else {
    return <TerminalIcon size={size} className="local-environment-icon" />;
  }
  return <img className="local-environment-icon" src={source} alt="" aria-hidden="true"
    title={name} data-environment={name} width={size} height={size} draggable={false} />;
}

export function LocalEnvironmentAvatar({ host, size = 18 }: Props) {
  return <span className="avatar local-avatar"><LocalEnvironmentIcon host={host} size={size} /></span>;
}
