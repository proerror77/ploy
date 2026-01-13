import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { api } from '@/services/api';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/Card';
import { Button } from '@/components/ui/Button';
import { Save, AlertCircle } from 'lucide-react';

export function StrategyConfig() {
  const queryClient = useQueryClient();
  const [formData, setFormData] = useState({
    symbols: '',
    min_move: 0.15,
    max_entry: 45,
    shares: 100,
    predictive: false,
    take_profit: 20,
    stop_loss: 12,
  });

  const { isLoading } = useQuery({
    queryKey: ['config'],
    queryFn: async () => {
      const data = await api.getConfig();
      setFormData({
        symbols: data.symbols.join(','),
        min_move: data.min_move,
        max_entry: data.max_entry,
        shares: data.shares,
        predictive: data.predictive,
        take_profit: data.take_profit ?? 20,
        stop_loss: data.stop_loss ?? 12,
      });
      return data;
    },
  });

  const updateMutation = useMutation({
    mutationFn: (data: typeof formData) =>
      api.updateConfig({
        symbols: data.symbols.split(',').map((s) => s.trim()),
        min_move: data.min_move,
        max_entry: data.max_entry,
        shares: data.shares,
        predictive: data.predictive,
        take_profit: data.take_profit,
        stop_loss: data.stop_loss,
      }),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['config'] });
      alert('配置保存成功！');
    },
    onError: (error) => {
      alert(`保存失败: ${error.message}`);
    },
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    updateMutation.mutate(formData);
  };

  if (isLoading) {
    return (
      <div className="flex h-full items-center justify-center">
        <div className="text-muted-foreground">加载中...</div>
      </div>
    );
  }

  return (
    <div className="p-8">
      <div className="mb-8">
        <h1 className="text-3xl font-bold">策略配置</h1>
        <p className="text-muted-foreground">调整交易策略参数</p>
      </div>

      <div className="grid grid-cols-1 gap-8 lg:grid-cols-3">
        <div className="lg:col-span-2">
          <Card>
            <CardHeader>
              <CardTitle>参数设置</CardTitle>
            </CardHeader>
            <CardContent>
              <form onSubmit={handleSubmit} className="space-y-6">
                <div>
                  <label className="mb-2 block text-sm font-medium">
                    交易标的 (逗号分隔)
                  </label>
                  <input
                    type="text"
                    value={formData.symbols}
                    onChange={(e) =>
                      setFormData({ ...formData, symbols: e.target.value })
                    }
                    className="w-full rounded-md border bg-background px-3 py-2"
                    placeholder="BTCUSDT,ETHUSDT,SOLUSDT"
                  />
                </div>

                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      最小移动百分比 (%)
                    </label>
                    <input
                      type="number"
                      step="0.01"
                      value={formData.min_move}
                      onChange={(e) =>
                        setFormData({ ...formData, min_move: parseFloat(e.target.value) })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    />
                  </div>

                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      最大入场价格 (%)
                    </label>
                    <input
                      type="number"
                      step="1"
                      value={formData.max_entry}
                      onChange={(e) =>
                        setFormData({ ...formData, max_entry: parseFloat(e.target.value) })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    />
                  </div>
                </div>

                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      每笔交易股数
                    </label>
                    <input
                      type="number"
                      value={formData.shares}
                      onChange={(e) =>
                        setFormData({ ...formData, shares: parseInt(e.target.value) })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    />
                  </div>

                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      预测模式
                    </label>
                    <select
                      value={formData.predictive ? 'true' : 'false'}
                      onChange={(e) =>
                        setFormData({ ...formData, predictive: e.target.value === 'true' })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    >
                      <option value="false">关闭</option>
                      <option value="true">开启</option>
                    </select>
                  </div>
                </div>

                <div className="grid grid-cols-2 gap-4">
                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      止盈百分比 (%)
                    </label>
                    <input
                      type="number"
                      step="1"
                      value={formData.take_profit}
                      onChange={(e) =>
                        setFormData({
                          ...formData,
                          take_profit: parseFloat(e.target.value),
                        })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    />
                  </div>

                  <div>
                    <label className="mb-2 block text-sm font-medium">
                      止损百分比 (%)
                    </label>
                    <input
                      type="number"
                      step="1"
                      value={formData.stop_loss}
                      onChange={(e) =>
                        setFormData({
                          ...formData,
                          stop_loss: parseFloat(e.target.value),
                        })
                      }
                      className="w-full rounded-md border bg-background px-3 py-2"
                    />
                  </div>
                </div>

                <Button type="submit" disabled={updateMutation.isPending}>
                  <Save className="mr-2 h-4 w-4" />
                  {updateMutation.isPending ? '保存中...' : '保存配置'}
                </Button>
              </form>
            </CardContent>
          </Card>
        </div>

        <div>
          <Card>
            <CardHeader>
              <CardTitle>参数说明</CardTitle>
            </CardHeader>
            <CardContent>
              <div className="space-y-4 text-sm">
                <div>
                  <div className="font-medium">最小移动百分比</div>
                  <div className="text-muted-foreground">
                    价格必须移动超过此百分比才会触发交易信号
                  </div>
                </div>
                <div>
                  <div className="font-medium">最大入场价格</div>
                  <div className="text-muted-foreground">
                    只在价格低于此百分比时入场
                  </div>
                </div>
                <div>
                  <div className="font-medium">每笔交易股数</div>
                  <div className="text-muted-foreground">
                    每次交易的固定股数
                  </div>
                </div>
                <div>
                  <div className="font-medium">预测模式</div>
                  <div className="text-muted-foreground">
                    启用机器学习预测增强交易信号
                  </div>
                </div>
                <div className="rounded-lg bg-warning/10 p-3">
                  <div className="flex gap-2">
                    <AlertCircle className="h-4 w-4 text-warning" />
                    <div className="text-warning">
                      修改配置需要重启交易系统才能生效
                    </div>
                  </div>
                </div>
              </div>
            </CardContent>
          </Card>
        </div>
      </div>
    </div>
  );
}
