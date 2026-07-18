import { useDeviceStatus } from '@api/deviceStatus.ts';
import { Chip, ChipProps } from '@mui/material';
import ErrorIcon from '@mui/icons-material/Error';
import CheckCircleIcon from '@mui/icons-material/CheckCircle';
import WarningIcon from '@mui/icons-material/Warning';

export default function WifiStrength() {
  const { data: deviceStatus } = useDeviceStatus();
  if (!deviceStatus || deviceStatus.wifiStrength === 0) return null;
  let color: ChipProps['color'] = 'success';
  let icon = <CheckCircleIcon />;
  if (deviceStatus.wifiStrength < 40) {
    color = 'error';
    icon = <ErrorIcon />;
  } else if (deviceStatus.wifiStrength < 70) {
    color = 'warning';
    icon = <WarningIcon />;
  }
  return (
    <Chip
      icon={ icon }
      color={ color }
      label={ `WiFi Strength ${deviceStatus.wifiStrength}%` }
      size='small'
      sx={ { mb: 1 } }
    />
  );
}
