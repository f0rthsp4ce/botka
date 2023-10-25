#!/usr/bin/env node

import * as d3 from "d3"
import fs from "fs"
import { JSDOM } from "jsdom"
import child_process from "child_process"
import path from "path"
import url from "url"

const USAGE = `
Usage: ${process.argv[1]} [options]
Options:
  -sqlite <filename>  Load data from sqlite3 database
  -json <filename>    Load data from JSON file
  -json-stdin         Load data from JSON file on stdin
  -out-svg <filename> Output SVG file (otherwise output to stdout)
`

async function parseArgs(args: string[]): Promise<{
  data: Resident[]
  outSvg: string | null
}> {
  let argi = 0
  let data
  let outSvg = null
  for (; argi < args.length; argi++) {
    if (args[argi] === "-sqlite") data = loadFromSqlite(args[++argi])
    else if (args[argi] === "-json")
      data = JSON.parse(await fs.promises.readFile(args[++argi], "utf-8"))
    else if (args[argi] === "-json-stdin")
      data = JSON.parse(fs.readFileSync(0, "utf-8"))
    else if (args[argi] === "-out-svg") outSvg = args[++argi]
    else {
      console.error(`Unknown argument: ${args[argi]}`)
      console.error(USAGE)
      process.exit(1)
    }
  }
  if (data === undefined) {
    console.error("No data source specified")
    console.error(USAGE)
    process.exit(1)
  }
  return { data, outSvg }
}

function projectDir(): string {
  let dir = path.dirname(url.fileURLToPath(import.meta.url))
  if (dir.endsWith("/dist")) dir = dir.slice(0, -"/dist".length)
  return dir
}

async function main() {
  const args = await parseArgs(process.argv.slice(2))
  const logo = await fs.promises.readFile(
    projectDir() + "/f0-logo.svg",
    "utf-8",
  )
  const jsDom = new JSDOM()
  const svg = d3
    .select(jsDom.window.document.body)
    .append("svg")
    .attr("xmlns", "http://www.w3.org/2000/svg")

  mkGraph(args.data, svg, logo)
  const output = svg.node()!.outerHTML
  if (args.outSvg !== null) await fs.promises.writeFile(args.outSvg, output)
  else console.log(output)
}

interface Resident {
  tg_id: number
  begin_date: string
  end_date: string | null
}

function loadFromSqlite(filename: string): Resident[] {
  const sqlite3 = child_process.spawnSync("sqlite3", [
    "-json",
    "--",
    `file:${filename}?mode=ro`,
    "SELECT tg_id, begin_date, end_date FROM residents",
  ])
  console.error(sqlite3.stderr.toString())
  const data = JSON.parse(sqlite3.stdout.toString())
  return data
}

function mkGraph(
  data0: Resident[],
  svg: d3.Selection<SVGSVGElement, unknown, null, undefined>,
  logo: string,
) {
  // Fix data
  const ids = new Map()
  const now = new Date()
  const data: { begin_date: Date; end_date: Date | null; no: number }[] = data0
    .sort((a, b) => a.begin_date.localeCompare(b.begin_date))
    .map((datum) => {
      let no = ids.get(datum.tg_id)
      if (no === undefined) {
        no = ids.size
        ids.set(datum.tg_id, no)
      }
      return {
        begin_date: new Date(datum.begin_date),
        end_date: datum.end_date === null ? null : new Date(datum.end_date),
        no,
      }
    })

  const svgWidth = 1024
  const svgHeight = 1024
  const margin = { top: 24, right: 24, bottom: 24, left: 24 }

  svg.attr("width", svgWidth).attr("height", svgHeight)

  // set background
  svg
    .append("rect")
    .attr("width", "100%")
    .attr("height", "100%")
    .attr("fill", "#241f31")

  const defs = svg.append("defs")
  addGradient(defs, "grad", [70, 130, 180, 1.0])
  addGradient(defs, "grad2", [120, 198, 120, 0.75])
  const white = "rgba(255, 255, 255, 0.5)"

  const xScale = d3
    .scaleTime()
    .domain([
      d3.min(data, (d) => d.begin_date)!,
      d3.max(data, (d) => d.end_date || now)!,
    ])
    .range([margin.left, svgWidth - margin.right])

  const yScale = d3
    .scaleLinear()
    .domain([0, d3.max(data, (d) => d.no)!])
    .range([svgHeight - margin.bottom - 220, margin.top])

  svg
    .append("g")
    .attr("transform", `translate(0,${svgHeight - margin.bottom})`)
    .call(d3.axisBottom(xScale))
    .style("color", white)
    .style("font-size", "16px")

  const data2 = []
  for (const datum of data) {
    data2.push({ d: datum.begin_date, v: +1 })
    if (datum.end_date !== null) {
      data2.push({ d: datum.end_date, v: -1 })
    }
  }
  data2.sort((a, b) => a.d.getTime() - b.d.getTime())
  let total = 0
  const data3 = data2.map((d) => ({ d: d.d, total: (total += d.v) }))
  data3.push({ d: now, total: total })

  const yScaleArea = d3
    .scaleLinear()
    .domain([0, d3.max(data3, (d) => d.total)!])
    .range([svgHeight - margin.bottom, svgHeight - margin.bottom - 200])
    .nice()

  svg
    .append("g")
    .attr("transform", `translate(${margin.left},0)`)
    .call(
      d3
        .axisLeft(yScaleArea)
        .ticks(3)
        .tickSize(-svgWidth + margin.left + margin.right)
        .tickSizeOuter(5),
    )
    .style("color", white)
    .style("font-size", "16px")

  const area = d3
    .area<{ d: Date; total: number }>()
    .x((d) => xScale(d.d))
    .y0(yScaleArea(0))
    .y1((d) => yScaleArea(d.total))
    .curve(d3.curveStepAfter)

  svg
    .append("path")
    .datum(data3)
    .attr("fill", "rgba(120, 198, 120, 0.75)")
    .attr("stroke", "black")
    .attr("stroke-width", 0)
    .attr("d", area)
  svg
    .append("rect")
    .attr("x", xScale(now))
    .attr("y", yScaleArea(total))
    .attr("width", margin.right)
    .attr("height", yScaleArea(0) - yScaleArea(total))
    .attr("fill", "url(#grad2)")

  data.forEach((datum) => {
    const height = yScale(0) - yScale(0.5)
    svg
      .append("rect")
      .attr("x", xScale(datum.begin_date))
      .attr("y", yScale(datum.no))
      .attr("width", xScale(datum.end_date || now) - xScale(datum.begin_date))
      .attr("height", height)
      .attr("fill", "rgba(70, 130, 180, 1.0)")
    if (datum.end_date === null) {
      svg
        .append("rect")
        .attr("x", xScale(now))
        .attr("y", yScale(datum.no))
        .attr("width", margin.right)
        .attr("height", height)
        .attr("fill", "url(#grad)")
    }
  })

  // Add verical lines for significant dates
  function event_(date: string, text: string, link: string): void {
    const date2 = new Date(date)
    svg
      .append("line")
      .attr("x1", xScale(date2))
      .attr("y1", margin.top)
      .attr("x2", xScale(date2))
      .attr("y2", svgHeight - margin.bottom)
      .attr("stroke-width", 1)
      .attr("stroke", white)
    svg
      .append("text")
      .attr("x", xScale(date2) + 10)
      .attr("y", margin.top + 10)
      .attr("font-size", 24)
      .text(text)
      .attr("fill", white)
    svg
      .append("text")
      .attr("x", xScale(date2) + 10)
      .attr("y", margin.top + 30)
      .attr("font-size", 16)
      .text(date2.toISOString().slice(0, 10))
      .attr("fill", white)
    return
    svg
      .append("a")
      .attr("xlink:href", link)
      .append("text")
      .attr("x", xScale(date2) + 10)
      .attr("y", margin.top + 50)
      .attr("font-size", 16)
      .text(link)
      .attr("fill", white)
  }
  event_("2022-03-26", "open", "t.me/f0rthsp4ce/8")
  event_("2023-03-26", "1y", "t.me/f0rthsp4ce/103")
  event_("2023-07-01", "$25", "t.me/f0rthsp4ce/170")

  // Add logo
  const jsDomLogo = JSDOM.fragment(logo)
  const logoSvg = jsDomLogo.firstChild as SVGElement
  logoSvg.setAttribute("width", "256")
  logoSvg.setAttribute("height", "256")
  ;(logoSvg.children[1] as SVGElement).style.fill = "#7c7"
  svg
    .append("g")
    .attr("transform", `translate(${margin.left + 64},${margin.top + 32})`)
    .append(() => logoSvg)
}

function addGradient(
  defs: d3.Selection<SVGDefsElement, unknown, null, undefined>,
  id: string,
  color: [number, number, number, number],
) {
  const grad = defs
    .append("linearGradient")
    .attr("id", id)
    .attr("x1", "0%")
    .attr("x2", "100%")
    .attr("y1", "50%")
    .attr("y2", "50%")
  grad
    .append("stop")
    .attr("offset", "0%")
    .style(
      "stop-color",
      `rgba(${color[0]}, ${color[1]}, ${color[2]}, ${color[3]})`,
    )
  grad
    .append("stop")
    .attr("offset", "100%")
    .style("stop-color", `rgba(${color[0]}, ${color[1]}, ${color[2]}, 0)`)
}

function addDays(date: Date, days: number): Date {
  const result = new Date(date)
  result.setDate(result.getDate() + days)
  return result
}

main()
