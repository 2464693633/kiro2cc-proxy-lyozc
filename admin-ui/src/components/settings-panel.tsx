// Copyright (c) 2026 Harllan He. Licensed under MIT.
import { useState, useEffect } from 'react'
import { toast } from 'sonner'
import { useTranslation } from 'react-i18next'
import { Card, CardContent } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import {
  useLoadBalancingMode, useSetLoadBalancingMode,
  useAuthKeys, useSetAuthKeys,
  useKeyPullConfig, useSetKeyPullConfig, useTestKeyPull,
} from '@/hooks/use-credentials'
import { extractErrorMessage } from '@/lib/utils'
import { LANG_STORAGE_KEY } from '@/i18n'
import type { TestKeyPullResponse } from '@/types/api'

export function SettingsPanel() {
  const { t, i18n } = useTranslation()
  const { data: loadBalancingData, isLoading: isLoadingMode } = useLoadBalancingMode()
  const { mutate: setLoadBalancingMode, isPending: isSettingMode } = useSetLoadBalancingMode()
  const { data: authKeysData, isLoading: isLoadingAuthKeys } = useAuthKeys()
  const { mutate: setAuthKeysMut, isPending: isSettingAuthKeys } = useSetAuthKeys()
  const [adminPswDraft, setAdminPswDraft] = useState('')
  const [editingAdminPsw, setEditingAdminPsw] = useState(false)

  // API Key 自动拉取
  const { data: keyPullData, isLoading: isLoadingKeyPull } = useKeyPullConfig()
  const { mutate: setKeyPullMut, isPending: isSettingKeyPull } = useSetKeyPullConfig()
  const { mutate: testKeyPullMut, isPending: isTesting } = useTestKeyPull()
  const [keyPullUrlDraft, setKeyPullUrlDraft] = useState('')
  const [editingKeyPullUrl, setEditingKeyPullUrl] = useState(false)
  const [intervalDraft, setIntervalDraft] = useState('')
  const [testResult, setTestResult] = useState<TestKeyPullResponse | null>(null)

  // 间隔输入框初值跟随后端返回。放在 effect 里而非 render 体内：
  // 直接在 render 里 setState 会每次渲染都触发一次额外渲染。
  // 依赖只含 intervalSecs，且仅在草稿为空时写入，避免覆盖用户正在输入的值。
  useEffect(() => {
    if (keyPullData?.intervalSecs !== undefined) {
      setIntervalDraft((prev) => (prev === '' ? String(keyPullData.intervalSecs) : prev))
    }
  }, [keyPullData?.intervalSecs])

  const changeLanguage = (lang: 'zh' | 'en') => {
    i18n.changeLanguage(lang)
    localStorage.setItem(LANG_STORAGE_KEY, lang)
  }

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">{t('settings.title')}</h2>

      <div className="space-y-6">
        {/* 认证密钥 */}
        <div className="space-y-1.5">
          <p className="px-1 text-xs font-medium text-muted-foreground">{t('settings.authKeys')}</p>
          <Card>
            <CardContent className="divide-y divide-border p-0">
              <div className="space-y-2 p-4">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">{t('settings.adminPassword')}</span>
                  {!editingAdminPsw && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => { setAdminPswDraft(''); setEditingAdminPsw(true) }}
                      disabled={isLoadingAuthKeys}
                    >
                      {t('common.edit')}
                    </Button>
                  )}
                </div>
                {editingAdminPsw ? (
                  <div className="flex gap-2">
                    <Input
                      type="text"
                      placeholder={t('settings.adminPasswordPlaceholder')}
                      value={adminPswDraft}
                      onChange={(e) => setAdminPswDraft(e.target.value)}
                      className="text-sm"
                    />
                    <Button
                      size="sm"
                      disabled={!adminPswDraft.trim() || isSettingAuthKeys}
                      onClick={() => {
                        setAuthKeysMut({ adminPsw: adminPswDraft.trim() }, {
                          onSuccess: () => {
                            toast.success(t('settings.adminPasswordUpdated'))
                            setEditingAdminPsw(false)
                            setAdminPswDraft('')
                          },
                          onError: (e) => toast.error(extractErrorMessage(e)),
                        })
                      }}
                    >
                      {t('common.save')}
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => setEditingAdminPsw(false)}>
                      {t('common.cancel')}
                    </Button>
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground font-mono">
                    {isLoadingAuthKeys ? t('common.loading') : authKeysData?.adminPsw ?? '—'}
                  </p>
                )}
              </div>
            </CardContent>
          </Card>
          <p className="px-1 text-xs text-muted-foreground">
            {t('settings.adminPasswordHint')}
          </p>
        </div>

        {/* 负载均衡 */}
        <div className="space-y-1.5">
          <p className="px-1 text-xs font-medium text-muted-foreground">{t('settings.loadBalancing')}</p>
          <Card>
            <CardContent className="p-0">
              <div className="flex items-center justify-between px-4 py-3">
                <span className="text-sm font-medium">{t('settings.loadBalancingMode')}</span>
                <Button
                  variant="outline"
                  size="sm"
                  onClick={() => {
                    const newMode = loadBalancingData?.mode === 'priority' ? 'balanced' : 'priority'
                    setLoadBalancingMode(newMode, {
                      onSuccess: () => toast.success(t('settings.switchedTo', {
                        mode: newMode === 'priority' ? t('settings.priorityMode') : t('settings.balancedMode'),
                      })),
                      onError: (e) => toast.error(extractErrorMessage(e)),
                    })
                  }}
                  disabled={isLoadingMode || isSettingMode}
                >
                  {isLoadingMode ? t('common.loading') : loadBalancingData?.mode === 'priority' ? t('settings.priorityMode') : t('settings.balancedMode')}
                </Button>
              </div>
            </CardContent>
          </Card>
        </div>

        {/* API Key 自动拉取 */}
        <div className="space-y-1.5">
          <p className="px-1 text-xs font-medium text-muted-foreground">{t('settings.keyPull')}</p>
          <Card>
            <CardContent className="divide-y divide-border p-0">
              {/* 开关 */}
              <div className="flex items-center justify-between px-4 py-3">
                <div className="space-y-0.5">
                  <span className="text-sm font-medium">{t('settings.keyPullEnabled')}</span>
                  <p className="text-xs text-muted-foreground">
                    {t('settings.keyPullEnabledHint')}
                  </p>
                </div>
                <Switch
                  checked={keyPullData?.enabled ?? false}
                  disabled={isLoadingKeyPull || isSettingKeyPull}
                  onCheckedChange={(checked) => {
                    setKeyPullMut({ enabled: checked }, {
                      onSuccess: () => toast.success(
                        checked ? t('settings.keyPullTurnedOn') : t('settings.keyPullTurnedOff')
                      ),
                      onError: (e) => toast.error(extractErrorMessage(e)),
                    })
                  }}
                />
              </div>

              {/* 拉取链接 */}
              <div className="space-y-2 p-4">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">{t('settings.keyPullUrl')}</span>
                  {!editingKeyPullUrl && (
                    <Button
                      variant="outline"
                      size="sm"
                      onClick={() => { setKeyPullUrlDraft(''); setEditingKeyPullUrl(true) }}
                      disabled={isLoadingKeyPull}
                    >
                      {t('common.edit')}
                    </Button>
                  )}
                </div>
                {editingKeyPullUrl ? (
                  <div className="flex gap-2">
                    <Input
                      type="text"
                      placeholder={t('settings.keyPullUrlPlaceholder')}
                      value={keyPullUrlDraft}
                      onChange={(e) => setKeyPullUrlDraft(e.target.value)}
                      className="text-sm"
                    />
                    <Button
                      size="sm"
                      disabled={!keyPullUrlDraft.trim() || isSettingKeyPull}
                      onClick={() => {
                        setKeyPullMut({ url: keyPullUrlDraft.trim() }, {
                          onSuccess: () => {
                            toast.success(t('settings.keyPullUrlUpdated'))
                            setEditingKeyPullUrl(false)
                            setKeyPullUrlDraft('')
                          },
                          onError: (e) => toast.error(extractErrorMessage(e)),
                        })
                      }}
                    >
                      {t('common.save')}
                    </Button>
                    <Button variant="ghost" size="sm" onClick={() => setEditingKeyPullUrl(false)}>
                      {t('common.cancel')}
                    </Button>
                  </div>
                ) : (
                  <p className="text-xs text-muted-foreground font-mono break-all">
                    {isLoadingKeyPull
                      ? t('common.loading')
                      : keyPullData?.urlConfigured
                        ? keyPullData.url
                        : t('settings.keyPullUrlNotSet')}
                  </p>
                )}
              </div>

              {/* 轮询间隔 */}
              <div className="space-y-2 p-4">
                <div className="flex items-center justify-between">
                  <span className="text-sm font-medium">{t('settings.keyPullInterval')}</span>
                  <div className="flex items-center gap-2">
                    <Input
                      type="number"
                      min={keyPullData?.minIntervalSecs ?? 5}
                      value={intervalDraft}
                      onChange={(e) => setIntervalDraft(e.target.value)}
                      className="w-24 text-sm"
                      disabled={isLoadingKeyPull}
                    />
                    <Button
                      size="sm"
                      disabled={isSettingKeyPull || !intervalDraft.trim()}
                      onClick={() => {
                        const secs = parseInt(intervalDraft, 10)
                        const min = keyPullData?.minIntervalSecs ?? 5
                        if (!Number.isFinite(secs) || secs < min) {
                          toast.error(t('settings.keyPullIntervalTooSmall', { min }))
                          return
                        }
                        setKeyPullMut({ intervalSecs: secs }, {
                          onSuccess: () => toast.success(t('settings.keyPullIntervalUpdated')),
                          onError: (e) => toast.error(extractErrorMessage(e)),
                        })
                      }}
                    >
                      {t('common.save')}
                    </Button>
                  </div>
                </div>
                <p className="text-xs text-muted-foreground">
                  {t('settings.keyPullIntervalHint', { min: keyPullData?.minIntervalSecs ?? 5 })}
                </p>
              </div>

              {/* 测试拉取 */}
              <div className="space-y-2 p-4">
                <div className="flex items-center justify-between">
                  <div className="space-y-0.5">
                    <span className="text-sm font-medium">{t('settings.keyPullTest')}</span>
                    <p className="text-xs text-muted-foreground">
                      {t('settings.keyPullTestHint')}
                    </p>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={!keyPullData?.urlConfigured || isTesting}
                    onClick={() => {
                      testKeyPullMut(undefined, {
                        onSuccess: (res) => {
                          setTestResult(res)
                          if (res.parsed === 0) {
                            toast.warning(t('settings.keyPullTestNoKeys'))
                          } else {
                            toast.success(t('settings.keyPullTestOk', { count: res.parsed }))
                          }
                        },
                        onError: (e) => {
                          setTestResult(null)
                          toast.error(extractErrorMessage(e))
                        },
                      })
                    }}
                  >
                    {isTesting ? t('common.loading') : t('settings.keyPullTestButton')}
                  </Button>
                </div>
                {testResult && testResult.keys.length > 0 && (
                  <div className="space-y-1 rounded-md bg-muted/50 p-2">
                    {testResult.keys.map((k, i) => (
                      <div key={i} className="flex items-center gap-3 text-xs">
                        <span className="font-mono">{k.maskedKey}</span>
                        <span className="text-muted-foreground">
                          {k.region ?? t('settings.keyPullRegionGlobal')}
                        </span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            </CardContent>
          </Card>
          <p className="px-1 text-xs text-muted-foreground">
            {t('settings.keyPullHint')}
          </p>
        </div>

        {/* 语言 */}
        <div className="space-y-1.5">
          <p className="px-1 text-xs font-medium text-muted-foreground">{t('settings.language')}</p>
          <Card>
            <CardContent className="p-0">
              <div className="flex items-center justify-between px-4 py-3">
                <span className="text-sm font-medium">{t('settings.language')}</span>
                <div className="flex gap-2">
                  <Button
                    variant={i18n.language === 'zh' ? 'default' : 'outline'}
                    size="sm"
                    onClick={() => changeLanguage('zh')}
                    aria-pressed={i18n.language === 'zh'}
                  >
                    {t('settings.languageZh')}
                  </Button>
                  <Button
                    variant={i18n.language === 'en' ? 'default' : 'outline'}
                    size="sm"
                    onClick={() => changeLanguage('en')}
                    aria-pressed={i18n.language === 'en'}
                  >
                    {t('settings.languageEn')}
                  </Button>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  )
}