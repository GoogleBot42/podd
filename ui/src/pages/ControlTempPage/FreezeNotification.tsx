import Alert from '@mui/material/Alert';
import { useDeviceStatus } from '@api/deviceStatus.ts';
import { useAppStore } from '@state/appStore.tsx';


// The freeze guard (podd, issue #186) has paused cooling on the selected side
// because its heat exchanger iced over. The side stays on (pump running) with
// its setpoint held just above the water meanwhile, so the dial shows that
// hold temperature; cooling resumes by itself and eases back to the target.
export default function FreezeNotification() {
  const { data: deviceStatus } = useDeviceStatus();
  const { side } = useAppStore();

  if (!deviceStatus?.[side]?.isThawing) {
    return null;
  }
  return (
    <Alert severity="info">
      Freeze protection: this side&apos;s heat exchanger iced over, so cooling is paused for a
      few minutes to thaw (water keeps circulating). Cooling resumes automatically.
    </Alert>
  );
}
