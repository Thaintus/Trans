param(
    [Parameter(Mandatory = $true)] [string] $Title
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[Windows.Forms.Application]::EnableVisualStyles()
$form = [Windows.Forms.Form]::new()
$form.Text = $Title
$form.ClientSize = [Drawing.Size]::new(320, 180)
$form.StartPosition = [Windows.Forms.FormStartPosition]::Manual
$form.Location = [Drawing.Point]::new(80, 80)
$form.BackColor = [Drawing.Color]::White
$form.ShowInTaskbar = $true
$form.TopMost = $false
$label = [Windows.Forms.Label]::new()
$label.Dock = [Windows.Forms.DockStyle]::Fill
$label.TextAlign = [Drawing.ContentAlignment]::MiddleCenter
$label.Text = 'Synthetic Trans transparency test window'
$form.Controls.Add($label)
try { [Windows.Forms.Application]::Run($form) }
finally { $form.Dispose() }
