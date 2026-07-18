import PrimeButton from './PrimeButton.tsx';
import PrimingNotification from './PrimingNotification.tsx';
import { useDeviceStatus } from '@api/deviceStatus.ts';

export default function PrimeControl() {
  const { data: deviceStatus, refetch } = useDeviceStatus();
  return (
    deviceStatus?.isPriming ?
      <PrimingNotification />
      :
      <PrimeButton refetch={ refetch }/>
  );
}

