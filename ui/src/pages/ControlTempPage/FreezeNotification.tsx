import Alert from '@mui/material/Alert';
import { useDeviceStatus } from '@api/deviceStatus.ts';
import { useAppStore } from '@state/appStore.tsx';


// The freeze guard (podd, issue #186) has switched the selected side off
// because its heat exchanger iced over. The side shows as off meanwhile; it
// comes back on by itself and eases back down to the target.
export default function FreezeNotification() {
  const { data: deviceStatus } = useDeviceStatus();
  const { side } = useAppStore();

  if (!deviceStatus?.[side]?.isThawing) {
    return null;
  }
  return (
    <Alert severity="info">
      Freeze protection: this side&apos;s heat exchanger iced over, so it is off for a few
      minutes to thaw. Cooling resumes automatically.
    </Alert>
  );
}
