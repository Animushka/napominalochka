import { useEffect, useMemo, useState, useSyncExternalStore } from 'react'
import { AliceController } from './alice-controller'

export function useAlice(ready: boolean, accountKey: string | number) {
  const controller = useMemo(() => new AliceController(), [accountKey])
  const state = useSyncExternalStore(controller.subscribe, controller.getSnapshot)
  const [now, setNow] = useState(Date.now)

  useEffect(() => {
    if (!ready) return
    controller.activate()
    const refresh = () => {
      if (document.visibilityState === 'visible') void controller.refresh()
    }
    window.addEventListener('focus', refresh)
    document.addEventListener('visibilitychange', refresh)
    return () => {
      window.removeEventListener('focus', refresh)
      document.removeEventListener('visibilitychange', refresh)
      controller.deactivate()
    }
  }, [controller, ready])

  useEffect(() => {
    if (state.cooldownUntil <= Date.now()) return
    setNow(Date.now())
    const interval = window.setInterval(() => {
      setNow(Date.now())
      if (Date.now() >= state.cooldownUntil) window.clearInterval(interval)
    }, 1000)
    return () => window.clearInterval(interval)
  }, [state.cooldownUntil])

  return { state, controller, remaining: Math.max(0, Math.ceil((state.cooldownUntil - Math.max(now, Date.now())) / 1000)) }
}
