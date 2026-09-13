# Reproduce the rs tile drawn in src/launcher/ui.rs, including its type and colors.
$ErrorActionPreference = 'Stop'
$assets = Join-Path (Split-Path -Parent $PSScriptRoot) 'assets'
if (-not (Test-Path -LiteralPath $assets -PathType Container)) { throw 'Assets directory does not exist.' }
Add-Type -AssemblyName System.Drawing
Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.IO;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.Drawing.Text;

public static class T7Logo {
    static Bitmap Render(int size) {
        // Supersample the same 48-unit tile used by the UI for smooth transparent edges.
        using (var canvas = new Bitmap(size * 4, size * 4, PixelFormat.Format32bppArgb)) {
            using (var g = Graphics.FromImage(canvas))
            using (var shape = new GraphicsPath())
            using (var orange = new SolidBrush(Color.FromArgb(206, 114, 60)))
            using (var charcoal = new SolidBrush(Color.FromArgb(21, 23, 25)))
            using (var font = new Font("Segoe UI Semibold", 28, FontStyle.Regular, GraphicsUnit.Pixel))
            using (var format = (StringFormat)StringFormat.GenericTypographic.Clone()) {
                if (font.FontFamily.Name != "Segoe UI Semibold") {
                    throw new InvalidOperationException("Segoe UI Semibold is required to reproduce the launcher logo.");
                }
                g.Clear(Color.Transparent);
                g.ScaleTransform(size * 4f / 48f, size * 4f / 48f);
                g.SmoothingMode = SmoothingMode.AntiAlias;
                g.TextRenderingHint = TextRenderingHint.AntiAliasGridFit;
                shape.AddArc(0, 0, 10, 10, 180, 90);
                shape.AddArc(38, 0, 10, 10, 270, 90);
                shape.AddArc(38, 38, 10, 10, 0, 90);
                shape.AddArc(0, 38, 10, 10, 90, 90);
                shape.CloseFigure();
                g.FillPath(orange, shape);
                format.LineAlignment = StringAlignment.Center;
                format.FormatFlags |= StringFormatFlags.NoWrap;
                g.DrawString("rs", font, charcoal, new RectangleF(9, 2, 36, 42), format);
            }
            var result = new Bitmap(size, size, PixelFormat.Format32bppArgb);
            using (var g = Graphics.FromImage(result)) {
                g.CompositingMode = CompositingMode.SourceCopy;
                g.InterpolationMode = InterpolationMode.HighQualityBicubic;
                g.PixelOffsetMode = PixelOffsetMode.HighQuality;
                g.DrawImage(canvas, new Rectangle(0, 0, size, size), 0, 0, canvas.Width, canvas.Height, GraphicsUnit.Pixel);
            }
            return result;
        }
    }

    public static void Generate(string directory) {
        using (var logo = Render(512)) {
            logo.Save(Path.Combine(directory, "t7patch-logo.png"), ImageFormat.Png);
        }
        int[] sizes = { 16, 24, 32, 48, 64, 128, 256 };
        var images = new byte[sizes.Length][];
        for (int i = 0; i < sizes.Length; i++) {
            using (var icon = Render(sizes[i]))
            using (var stream = new MemoryStream()) {
                icon.Save(stream, ImageFormat.Png);
                images[i] = stream.ToArray();
            }
        }
        File.WriteAllBytes(Path.Combine(directory, "t7patch-icon.png"), images[images.Length - 1]);
        using (var writer = new BinaryWriter(File.Create(Path.Combine(directory, "t7patch.ico")))) {
            writer.Write((ushort)0);
            writer.Write((ushort)1);
            writer.Write((ushort)sizes.Length);
            uint offset = (uint)(6 + sizes.Length * 16);
            for (int i = 0; i < sizes.Length; i++) {
                writer.Write((byte)(sizes[i] == 256 ? 0 : sizes[i]));
                writer.Write((byte)(sizes[i] == 256 ? 0 : sizes[i]));
                writer.Write((byte)0);
                writer.Write((byte)0);
                writer.Write((ushort)1);
                writer.Write((ushort)32);
                writer.Write((uint)images[i].Length);
                writer.Write(offset);
                offset += (uint)images[i].Length;
            }
            foreach (var image in images) { writer.Write(image); }
        }
    }
}
'@
[T7Logo]::Generate($assets)
Write-Output "Generated: $assets\t7patch-logo.png, t7patch-icon.png and t7patch.ico"
