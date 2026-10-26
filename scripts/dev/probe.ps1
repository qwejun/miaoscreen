# Connects to the publisher and reports what arrives, without rendering anything.
param(
    [string]$Address = '127.0.0.1',
    [int]$Port = 7391,
    [int]$Messages = 12
)

$ErrorActionPreference = 'Stop'
$client = New-Object System.Net.Sockets.TcpClient
$client.Connect($Address, $Port)
$client.NoDelay = $true
$stream = $client.GetStream()
Write-Host "connected to ${Address}:${Port}"

function Read-Exact([System.IO.Stream]$s, [int]$count) {
    $buffer = New-Object byte[] $count
    $read = 0
    while ($read -lt $count) {
        $n = $s.Read($buffer, $read, $count - $read)
        if ($n -le 0) { throw 'connection closed' }
        $read += $n
    }
    return ,$buffer
}

for ($i = 0; $i -lt $Messages; $i++) {
    $header = Read-Exact $stream 5
    $kind = $header[0]
    $length = [BitConverter]::ToUInt32($header, 1)
    $payload = if ($length -gt 0) { Read-Exact $stream ([int]$length) } else { @() }

    switch ($kind) {
        0x10 {
            $version = [BitConverter]::ToUInt32($payload, 0)
            Write-Host ("HELLO      version={0} len={1}" -f $version, $length)
        }
        0x11 {
            $version = [BitConverter]::ToUInt32($payload, 0)
            $generation = [BitConverter]::ToUInt64($payload, 4)
            $offset = 12
            $hasBackground = $payload[$offset]
            $offset += 1
            $backgroundBytes = 0
            if ($hasBackground -ne 0) {
                $backgroundBytes = [BitConverter]::ToUInt32($payload, ($offset + 20))
                $offset += 24 + [int]$backgroundBytes
            }
            $textures = [BitConverter]::ToUInt32($payload, $offset)
            $offset += 4
            $bytes = 0
            for ($t = 0; $t -lt $textures; $t++) {
                $id = [BitConverter]::ToUInt32($payload, $offset)
                $w = [BitConverter]::ToUInt32($payload, ($offset + 4))
                $h = [BitConverter]::ToUInt32($payload, ($offset + 8))
                $size = [BitConverter]::ToUInt32($payload, ($offset + 12))
                $offset += 16 + [int]$size
                $bytes += [int]$size
                Write-Host ("  texture id={0} {1}x{2} {3:N0} bytes" -f $id, $w, $h, $size)
            }
            $keys = [BitConverter]::ToUInt32($payload, $offset)
            $offset += 4
            for ($k = 0; $k -lt $keys; $k++) {
                $size = [BitConverter]::ToUInt32($payload, ($offset + 29))
                $offset += 33 + [int]$size
            }
            Write-Host ("RESOURCES  gen={0} background={1:N0} textures={2} png={3:N0} bytes keyAssets={4} total={5:N0} bytes" -f $generation, $backgroundBytes, $textures, $bytes, $keys, $length)
        }
        0x12 {
            $generation = [BitConverter]::ToUInt64($payload, 4)
            $sequence = [BitConverter]::ToUInt64($payload, 12)
            $opacity = [BitConverter]::ToSingle($payload, 20)
            $mirror = $payload[24]
            $minX = [BitConverter]::ToSingle($payload, 25)
            $maxX = [BitConverter]::ToSingle($payload, 29)
            $minY = [BitConverter]::ToSingle($payload, 33)
            $maxY = [BitConverter]::ToSingle($payload, 37)
            $offset = 41
            $keyCount = [BitConverter]::ToUInt32($payload, $offset)
            $offset += 4 + (5 * [int]$keyCount)
            $drawables = [BitConverter]::ToUInt32($payload, $offset)
            $offset += 4
            $vertices = 0
            $indices = 0
            $masked = 0
            for ($d = 0; $d -lt $drawables; $d++) {
                $offset += 4 + 4 + 4 + 1 + 4 + 16 + 16
                $maskCount = [BitConverter]::ToUInt16($payload, $offset)
                $offset += 2 + (4 * [int]$maskCount)
                if ($maskCount -gt 0) { $masked++ }
                $vertexCount = [BitConverter]::ToUInt16($payload, $offset)
                $offset += 2 + (16 * [int]$vertexCount)
                $vertices += $vertexCount
                $indexCount = [BitConverter]::ToUInt16($payload, $offset)
                $offset += 2 + (2 * [int]$indexCount)
                $indices += $indexCount
            }
            Write-Host ("FRAME      gen={0} seq={1} opacity={2} mirror={3} bounds=[{4:N2},{5:N2}]x[{6:N2},{7:N2}] keys={8} drawables={9} masked={10} verts={11} idx={12} {13:N0} bytes hash={14}" -f `
                $generation, $sequence, $opacity, $mirror, $minX, $maxX, $minY, $maxY, $keyCount, $drawables, $masked, $vertices, $indices, $length, `
                ([BitConverter]::ToString([Security.Cryptography.MD5]::Create().ComputeHash($payload), 0, 4)))
        }
        default {
            Write-Host ("UNKNOWN    kind=0x{0:X2} len={1}" -f $kind, $length)
        }
    }
}

$client.Close()
Write-Host 'done'
