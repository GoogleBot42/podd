import InfoIcon from '@mui/icons-material/Info';
import { Box, CircularProgress, FormControlLabel, Typography, Switch } from '@mui/material';
import Section from '../Section.tsx';
import { Services, useServices, postServices } from '@api/services.ts';
import { useAppStore } from '@state/appStore.tsx';
import { DeepPartial } from 'ts-essentials';

export default function FeaturesSection() {
  const { data: services, refetch, isLoading } = useServices();
  const setIsUpdating = useAppStore(state => state.setIsUpdating);

  const updateServices = (services: DeepPartial<Services>) => {
    setIsUpdating(true);

    postServices(services)
      .then(() => refetch())
      .catch(error => {
        console.error(error);
      })
      .finally(() => setIsUpdating(false));
  };

  if (isLoading || !services) return <CircularProgress />;

  return (
    <Section title='Features'>
      <FormControlLabel
        control={
          <Switch
            // podd persists nothing for this service, so the switch used to
            // bounce straight back on the next refetch.
            disabled
            checked={ services.biometrics.enabled }
            onChange={ (event) => updateServices({ biometrics: { enabled: event.target.checked } }) }
          />
        }
        label="Biometrics"
      />
      <Box display='flex' gap={ 1 }>

        <InfoIcon sx={ { color: 'text.secondary' } }/>

        <Box>
          <Typography color='text.secondary'>
            Calculate biometrics (heart rate, breathing, HRV) from the bed sensors.
          </Typography>
          <Typography variant='body2' color='text.secondary'>
            Not yet supported by podd
          </Typography>
        </Box>

      </Box>
    </Section>
  );
}
