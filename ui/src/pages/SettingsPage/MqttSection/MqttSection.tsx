import { useEffect, useState } from 'react';
import {
  Alert,
  Box,
  Button,
  CircularProgress,
  FormControlLabel,
  Snackbar,
  Switch,
  TextField,
  Typography,
} from '@mui/material';
import InfoIcon from '@mui/icons-material/Info';

import Section from '../Section.tsx';
import { MqttSettings, MqttSettingsPatch, postMqtt, useMqtt } from '@api/mqtt.ts';
import { useAppStore } from '@state/appStore.tsx';

// Kept blank unless the user types one: an empty password field means "keep
// whatever is stored", which is why the API never has to send it back.
const BLANK_PASSWORD = '';

type FormState = {
  enabled: boolean;
  server: string;
  port: string;
  user: string;
};

const toForm = (mqtt?: MqttSettings): FormState => ({
  enabled: mqtt?.enabled ?? false,
  server: mqtt?.server ?? '',
  port: String(mqtt?.port ?? 1883),
  user: mqtt?.user ?? '',
});

export default function MqttSection() {
  const { data: mqtt, refetch, isLoading } = useMqtt();
  const { isUpdating, setIsUpdating } = useAppStore();
  const [form, setForm] = useState<FormState>(toForm(mqtt));
  const [password, setPassword] = useState(BLANK_PASSWORD);
  const [clearPassword, setClearPassword] = useState(false);
  const [saveError, setSaveError] = useState('');
  const [saved, setSaved] = useState(false);

  useEffect(() => {
    setForm(toForm(mqtt));
    setPassword(BLANK_PASSWORD);
    setClearPassword(false);
  }, [mqtt]);

  if (isLoading) return <CircularProgress />;

  const port = Number(form.port);
  const portError = !Number.isInteger(port) || port < 1 || port > 65535;
  const serverError = form.enabled && form.server.trim() === '';
  const dirty = form.enabled !== (mqtt?.enabled ?? false)
    || form.server !== (mqtt?.server ?? '')
    || port !== (mqtt?.port ?? 0)
    || form.user !== (mqtt?.user ?? '')
    || password !== BLANK_PASSWORD
    || clearPassword;

  const handleSave = () => {
    if (portError || serverError) return;
    const patch: MqttSettingsPatch = {
      enabled: form.enabled,
      server: form.server.trim(),
      port,
      user: form.user,
    };
    if (password !== BLANK_PASSWORD) {
      patch.password = password;
    } else if (clearPassword) {
      patch.password = '';
    }

    setIsUpdating(true);
    postMqtt(patch)
      .then(() => refetch())
      .then(() => {
        setPassword(BLANK_PASSWORD);
        setClearPassword(false);
        setSaved(true);
      })
      .catch(error => {
        console.error(error);
        const details = error?.response?.data?.details;
        setSaveError(Array.isArray(details) ? details.join('; ') : 'Failed to save MQTT settings');
      })
      .finally(() => setIsUpdating(false));
  };

  const passwordHelp = clearPassword
    ? 'Will be cleared when you save'
    : mqtt?.passwordSet
      ? 'A password is stored. Leave blank to keep it.'
      : 'No password stored';

  return (
    <Section title='MQTT'>
      <Box display='flex' flexDirection='column' gap={ 2 }>
        <FormControlLabel
          control={
            <Switch
              checked={ form.enabled }
              disabled={ isUpdating }
              onChange={ (event) => setForm({ ...form, enabled: event.target.checked }) }
            />
          }
          label='Publish to an MQTT broker'
        />

        <TextField
          label='Broker host'
          placeholder='homeassistant.local'
          value={ form.server }
          error={ serverError }
          helperText={ serverError ? 'Required while MQTT is enabled' : 'Host name or IP — no scheme, no port' }
          disabled={ isUpdating }
          onChange={ (event) => setForm({ ...form, server: event.target.value }) }
          fullWidth
        />

        <TextField
          label='Port'
          type='number'
          value={ form.port }
          error={ portError }
          helperText={ portError ? 'Must be between 1 and 65535' : ' ' }
          disabled={ isUpdating }
          onChange={ (event) => setForm({ ...form, port: event.target.value }) }
          fullWidth
        />

        <TextField
          label='Username'
          value={ form.user }
          helperText='Leave blank for an anonymous broker'
          disabled={ isUpdating }
          onChange={ (event) => setForm({ ...form, user: event.target.value }) }
          fullWidth
        />

        <TextField
          label='Password'
          type='password'
          value={ password }
          placeholder={ mqtt?.passwordSet ? '••••••••' : '' }
          helperText={ passwordHelp }
          disabled={ isUpdating || clearPassword }
          autoComplete='new-password'
          onChange={ (event) => setPassword(event.target.value) }
          fullWidth
        />

        <Box display='flex' gap={ 1 } alignItems='center'>
          <Button
            variant='contained'
            disabled={ isUpdating || !dirty || portError || serverError }
            onClick={ handleSave }
          >
            Save
          </Button>
          { mqtt?.passwordSet && !clearPassword && (
            <Button
              size='small'
              color='inherit'
              disabled={ isUpdating }
              onClick={ () => {
                setPassword(BLANK_PASSWORD);
                setClearPassword(true);
              } }
            >
              Clear password
            </Button>
          ) }
          { clearPassword && (
            <Button size='small' color='inherit' onClick={ () => setClearPassword(false) }>
              Keep password
            </Button>
          ) }
        </Box>

        <Box display='flex' gap={ 1 }>
          <InfoIcon sx={ { color: 'text.secondary' } }/>
          <Box>
            <Typography color='text.secondary'>
              Publishes bed state and Home Assistant discovery to your broker, and accepts
              control actions back. The password is stored on the Pod and never sent back to
              this page.
            </Typography>
            <Typography variant='body2' color='text.secondary'>
              The broker connection is made when podd starts, so a change here takes effect
              after the next restart.
            </Typography>
          </Box>
        </Box>
      </Box>

      <Snackbar
        open={ saveError !== '' }
        autoHideDuration={ 6000 }
        onClose={ () => setSaveError('') }
      >
        <Alert severity='error' onClose={ () => setSaveError('') }>
          { saveError }
        </Alert>
      </Snackbar>
      <Snackbar
        open={ saved }
        autoHideDuration={ 4000 }
        onClose={ () => setSaved(false) }
      >
        <Alert severity='success' onClose={ () => setSaved(false) }>
          MQTT settings saved — restart podd to reconnect
        </Alert>
      </Snackbar>
    </Section>
  );
}
